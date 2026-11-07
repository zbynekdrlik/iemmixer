// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/acme/tests.rs (iemmixer#91).
//! Tests of `acme.rs`: the real ACME client (instant-acme) against the
//! local ACME double (`acme/double.rs`) and the Cloudflare DNS API double
//! (`cloudflare::double`), the keeper with a real HTTPS listener, and its
//! pure decisions.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum_server::tls_rustls::RustlsConfig;
use instant_acme::RetryPolicy;

use super::double::{self as ca_double, SharedCa};
use super::*;
use crate::cloudflare::double::{self as cf, Shared as CfApi};
use crate::tls_cert::test_certs::{TestCa, handshake};
use crate::tls_cert::{CertFiles, CertInfo};

const NAME: &str = "mixer.example.org";
const DAY: i64 = 86_400;

fn cf_secret() -> String {
    "c".repeat(24)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Stores the doubles' Cloudflare token in `dir`, as the keeper finds it.
fn store_token(dir: &std::path::Path) {
    let token = crate::cf_token::read_token(format!("{}\n", cf_secret()).as_bytes()).unwrap();
    crate::cf_token::store(dir, &token).unwrap();
}

/// The two doubles and an ACME client of NAME pointed at them, with the
/// Cloudflare token set in `dir` when `set_token`.
async fn setup(dir: &std::path::Path, set_token: bool) -> (Acme, CfApi, SharedCa) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cf_base, cf_api) = cf::start(&cf_secret(), &[("example.org", "zone-1")]).await;
    let (directory, ca) = ca_double::start(Arc::clone(&cf_api)).await;
    if set_token {
        store_token(dir);
    }
    let cfg = AcmeConfig {
        contact: Some("owner@example.org".into()),
        directory,
        propagation_s: 0,
    };
    let files = CertFiles::new(dir, "cert.pem", "key.pem");
    let mut acme = Acme::new(NAME, &cfg, dir, files).unwrap();
    assert_eq!(acme.cloudflare_api, crate::cloudflare::API);
    assert_eq!(acme.attempt_limit, ATTEMPT_LIMIT);
    acme.cloudflare_api = cf_base;
    acme.retry = RetryPolicy::new()
        .initial_delay(Duration::from_millis(10))
        .timeout(Duration::from_secs(5));
    (acme, cf_api, ca)
}

fn count(ca: &SharedCa, request: &str) -> usize {
    ca.lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.as_str() == request)
        .count()
}

/// An HTTPS listener on 127.0.0.1 serving `pem` from a `RustlsConfig` the
/// keeper can reload: the config, the address and the handle that stops it.
async fn listener(pem: &Pem) -> (RustlsConfig, SocketAddr, axum_server::Handle<SocketAddr>) {
    let rustls =
        RustlsConfig::from_pem(pem.chain.clone().into_bytes(), pem.key.clone().into_bytes())
            .await
            .unwrap();
    let handle = axum_server::Handle::new();
    let (served, stop) = (rustls.clone(), handle.clone());
    tokio::spawn(async move {
        axum_server::bind_rustls("127.0.0.1:0".parse().unwrap(), served)
            .handle(stop)
            .serve(axum::Router::new().into_make_service())
            .await
    });
    let addr = handle.listening().await.unwrap();
    (rustls, addr, handle)
}

/// A certificate of NAME from a CA of its own, `left` days to go: that CA
/// and the pair.
fn by_hand(left: i64) -> (TestCa, Pem) {
    let own = TestCa::new();
    let (chain, key) = own.leaf(&[NAME], now() - 80 * DAY, now() + left * DAY);
    let pem = crate::tls_cert::checked(chain, key, NAME).unwrap();
    (own, pem)
}

fn status_of(cell: &StatusCell) -> CertStatus {
    cell.read().unwrap().clone().unwrap()
}

#[tokio::test]
async fn a_certificate_comes_by_dns01_and_the_record_goes_again() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, cf_api, ca) = setup(dir.path(), true).await;
    let pem = acme.issue().await.unwrap();
    assert_eq!(pem.info.names, vec![NAME.to_string()]);
    assert!(pem.info.not_after > now());
    assert!(pem.chain.contains("BEGIN CERTIFICATE"));
    assert!(pem.key.contains("PRIVATE KEY"));
    // The challenge record was written, checked by the CA, and removed.
    {
        let cf_state = cf_api.lock().unwrap();
        assert!(cf_state.records.is_empty(), "no record stays");
        assert!(
            cf_state
                .calls
                .iter()
                .any(|c| c == "POST /zones/zone-1/dns_records"),
            "{:?}",
            cf_state.calls
        );
        assert!(
            cf_state
                .calls
                .iter()
                .any(|c| c.starts_with("DELETE /zones/zone-1/dns_records/rec"))
        );
    }
    let accounts = ca.lock().unwrap().new_accounts.clone();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0]["contact"],
        serde_json::json!(["mailto:owner@example.org"])
    );
    assert!(acme.account_path().is_file(), "the account is stored");
    // `issue` stores nothing itself.
    assert!(!acme.files.cert.exists());
    // A second certificate reuses the stored account, and the CA reuses
    // the account's valid authorization: no new record, no challenge.
    acme.issue().await.unwrap();
    assert_eq!(count(&ca, "POST /new-account"), 1);
    assert_eq!(count(&ca, "POST /new-order"), 2);
    let posts = cf_api
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|c| *c == "POST /zones/zone-1/dns_records")
        .count();
    assert_eq!(posts, 1);
    let challenges = ca
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.starts_with("POST /chall/"))
        .count();
    assert_eq!(challenges, 1);
}

#[tokio::test]
async fn a_record_a_crashed_attempt_left_is_removed_first() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, cf_api, _ca) = setup(dir.path(), true).await;
    let fqdn = format!("_acme-challenge.{NAME}");
    let ours = crate::cloudflare::TXT_COMMENT.to_string();
    cf_api
        .lock()
        .unwrap()
        .records
        .insert("stale".into(), (fqdn.clone(), "old-value".into(), ours));
    // A record another client (another ACME client) made is left alone.
    cf_api.lock().unwrap().records.insert(
        "foreign".into(),
        (fqdn.clone(), "theirs".into(), "certbot".into()),
    );
    acme.issue().await.unwrap();
    let records = cf_api.lock().unwrap().records.clone();
    assert_eq!(records.keys().collect::<Vec<_>>(), vec!["foreign"]);
    let calls = cf_api.lock().unwrap().calls.clone();
    assert!(calls.contains(&"DELETE /zones/zone-1/dns_records/stale".to_string()));
    assert!(!calls.contains(&"DELETE /zones/zone-1/dns_records/foreign".to_string()));
    // The leftover went before the CA was asked for anything.
    let stale = calls
        .iter()
        .position(|c| c == "DELETE /zones/zone-1/dns_records/stale")
        .unwrap();
    let post = calls
        .iter()
        .position(|c| c == "POST /zones/zone-1/dns_records")
        .unwrap();
    assert!(stale < post, "{calls:?}");
}

#[tokio::test]
async fn a_refused_challenge_is_an_error_and_the_record_still_goes() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, cf_api, ca) = setup(dir.path(), true).await;
    ca.lock().unwrap().refuse_challenges = true;
    let error = acme.issue().await.unwrap_err();
    assert!(error.starts_with("ACME challenge: "), "{error}");
    assert!(
        error.contains("no TXT record with the key authorization"),
        "{error}"
    );
    assert!(
        cf_api.lock().unwrap().records.is_empty(),
        "removed after the failure too"
    );
    assert!(!acme.files.cert.exists());
}

#[tokio::test]
async fn without_a_token_nothing_is_asked_of_anyone() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, cf_api, ca) = setup(dir.path(), false).await;
    assert_eq!(acme.issue().await.unwrap_err(), NO_TOKEN);
    assert!(cf_api.lock().unwrap().calls.is_empty());
    assert!(ca.lock().unwrap().requests.is_empty());
    assert!(NO_TOKEN.contains("iem-server cloudflare set-token"));
}

#[tokio::test]
async fn cloudflare_errors_stop_the_attempt_before_the_ca() {
    let dir = tempfile::tempdir().unwrap();
    let (mut acme, cf_api, ca) = setup(dir.path(), true).await;
    acme.name = "mixer.elsewhere.org".into();
    let error = acme.issue().await.unwrap_err();
    assert!(
        error.starts_with("no Cloudflare zone for mixer.elsewhere.org"),
        "{error}"
    );
    acme.name = NAME.into();
    cf_api.lock().unwrap().fail = Some((500, "Internal error".into()));
    let error = acme.issue().await.unwrap_err();
    assert!(error.ends_with("HTTP 500: Internal error"), "{error}");
    assert!(ca.lock().unwrap().requests.is_empty());
    // A token file that is no token.
    crate::sealed::write(
        &crate::cf_token::token_path(&dir.path().join(crate::secrets::SECRETS_DIR)),
        b"no",
    )
    .unwrap();
    assert!(
        acme.issue()
            .await
            .unwrap_err()
            .contains("does not hold a Cloudflare API token")
    );
}

#[tokio::test]
async fn another_directory_or_a_corrupt_file_gets_a_new_account() {
    let dir = tempfile::tempdir().unwrap();
    let (mut acme, cf_api, ca) = setup(dir.path(), true).await;
    acme.cfg.contact = None;
    acme.issue().await.unwrap();
    let accounts = ca.lock().unwrap().new_accounts.clone();
    assert_eq!(accounts[0]["contact"], serde_json::json!([]));
    // Another CA (staging → production): a new account there.
    let (other, other_ca) = ca_double::start(Arc::clone(&cf_api)).await;
    acme.cfg.directory = other;
    acme.issue().await.unwrap();
    assert_eq!(count(&other_ca, "POST /new-account"), 1);
    // Back to the first CA: the stored account is the other CA's now.
    let first = ca.lock().unwrap().base.clone();
    acme.cfg.directory = format!("{first}/directory");
    std::fs::write(acme.account_path(), b"{not json").unwrap();
    acme.issue().await.unwrap();
    assert_eq!(count(&ca, "POST /new-account"), 2);
    // An account file that cannot be read is an error, never replaced.
    std::fs::remove_file(acme.account_path()).unwrap();
    std::fs::create_dir(acme.account_path()).unwrap();
    assert!(
        acme.issue()
            .await
            .unwrap_err()
            .starts_with("reading the ACME account")
    );
}

#[test]
fn an_https_directory_uses_the_verified_client() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let cfg = AcmeConfig {
        contact: None,
        directory: "https://acme-staging-v02.api.letsencrypt.org/directory".into(),
        propagation_s: 20,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _inside = runtime.enter();
    let files = CertFiles::new(dir.path(), "cert.pem", "key.pem");
    let acme = Acme::new(NAME, &cfg, dir.path(), files).unwrap();
    assert!(acme.builder().is_ok());
    assert_eq!(
        format!("{:?}", acme.retry),
        format!("{:?}", RetryPolicy::new().timeout(CA_CHECK))
    );
}

#[tokio::test]
async fn the_keeper_gets_stores_and_serves_a_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, _cf, ca) = setup(dir.path(), true).await;
    let files = acme.files.clone();
    // The stand-in the listener starts on while nothing is stored.
    let (rustls, addr, handle) = listener(&crate::tls_cert::stand_in(NAME).unwrap()).await;
    let status: StatusCell = Arc::default();
    let task = tokio::spawn(keep(acme, rustls, Arc::clone(&status)));
    let ca_pem = ca.lock().unwrap().test_ca.pem.clone();
    let mut served = Err(String::new());
    for _ in 0..500 {
        served = handshake(addr, NAME, &ca_pem).await;
        if served.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();
    let served = served.expect("the CA's certificate is served");
    assert_eq!(served.names, vec![NAME.to_string()]);
    assert!(served.not_after > now() + 30 * DAY, "{served:?}");
    // Stored where the site's files are, and the status says so.
    match stored(&files, NAME) {
        Stored::Serve(pem) => assert_eq!(pem.info, served),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(served.not_after),
            last_error: None,
            failures: 0,
        }
    );
    handle.shutdown();
}

#[tokio::test]
async fn a_failed_attempt_is_counted_and_keeps_the_served_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, _cf, ca) = setup(dir.path(), false).await;
    // The old certificate, 10 days left, is stored and served.
    let (old_ca, old) = by_hand(10);
    acme.files.save(&old).unwrap();
    let (rustls, addr, handle) = listener(&old).await;
    let status: StatusCell = Arc::default();
    // No token: retried every 5 minutes, the old certificate stays.
    assert_eq!(
        acme.check(&rustls, &status, now()).await,
        NO_TOKEN_RETRY,
        "a due certificate is renewed"
    );
    assert_eq!(acme.renew(&rustls, &status).await, Err(NO_TOKEN_RETRY));
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(old.info.not_after),
            last_error: Some(NO_TOKEN.to_string()),
            failures: 2,
        }
    );
    let served = handshake(addr, NAME, &old_ca.pem).await.unwrap();
    assert_eq!(served, old.info);
    // With the token set, the next attempt succeeds, resets the count and
    // serves the CA's certificate.
    store_token(dir.path());
    assert_eq!(acme.renew(&rustls, &status).await, Ok(()));
    let ca_pem = ca.lock().unwrap().test_ca.pem.clone();
    let served = handshake(addr, NAME, &ca_pem).await.unwrap();
    assert!(served.not_after > now() + 30 * DAY, "{served:?}");
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(served.not_after),
            last_error: None,
            failures: 0,
        }
    );
    // A refused challenge (a new authorization: another account) is an
    // error retried after a minute, then two; the new certificate stays
    // served and stored.
    std::fs::remove_file(acme.account_path()).unwrap();
    ca.lock().unwrap().refuse_challenges = true;
    assert_eq!(acme.renew(&rustls, &status).await, Err(FIRST_RETRY));
    assert_eq!(
        acme.renew(&rustls, &status).await,
        Err(Duration::from_secs(120))
    );
    assert_eq!(handshake(addr, NAME, &ca_pem).await.unwrap(), served);
    match stored(&acme.files, NAME) {
        Stored::Serve(pem) => assert_eq!(pem.info, served),
        other => panic!("{other:?}"),
    }
    let after = status_of(&status);
    assert_eq!((after.expires, after.failures), (Some(served.not_after), 2));
    assert!(
        after
            .last_error
            .unwrap()
            .contains("no TXT record with the key authorization")
    );
    handle.shutdown();
}

#[tokio::test]
async fn a_certificate_with_thirty_days_left_is_kept_and_nobody_is_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, cf_api, ca) = setup(dir.path(), true).await;
    let (own_ca, kept) = by_hand(60);
    acme.files.save(&kept).unwrap();
    let (rustls, addr, handle) = listener(&kept).await;
    let status: StatusCell = Arc::default();
    assert_eq!(acme.check(&rustls, &status, now()).await, CHECK_EVERY);
    assert!(cf_api.lock().unwrap().calls.is_empty());
    assert!(ca.lock().unwrap().requests.is_empty());
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(kept.info.not_after),
            last_error: None,
            failures: 0,
        }
    );
    assert_eq!(handshake(addr, NAME, &own_ca.pem).await.unwrap(), kept.info);
    handle.shutdown();
}

#[tokio::test]
async fn a_stored_certificate_of_another_name_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (acme, _cf, ca) = setup(dir.path(), true).await;
    let other = TestCa::new();
    let (chain, key) = other.leaf(&["old.example.org"], now() - DAY, now() + 60 * DAY);
    std::fs::write(&acme.files.cert, &chain).unwrap();
    std::fs::write(&acme.files.key, &key).unwrap();
    assert!(matches!(stored(&acme.files, NAME), Stored::Unusable(_)));
    let (rustls, addr, handle) = listener(&crate::tls_cert::stand_in(NAME).unwrap()).await;
    let status: StatusCell = Arc::default();
    // A failure keeps the count while no usable certificate is stored.
    record_failure(&status, "earlier".to_string());
    assert_eq!(acme.check(&rustls, &status, now()).await, CHECK_EVERY);
    let ca_pem = ca.lock().unwrap().test_ca.pem.clone();
    let served = handshake(addr, NAME, &ca_pem).await.unwrap();
    assert_eq!(served.names, vec![NAME.to_string()]);
    assert_eq!(status_of(&status).failures, 0);
    handle.shutdown();
}

#[tokio::test]
async fn an_attempt_that_never_ends_is_cut_off() {
    let dir = tempfile::tempdir().unwrap();
    let (mut acme, _cf, _ca) = setup(dir.path(), true).await;
    // The CA takes the connection and never answers.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    acme.cfg.directory = format!("http://{}/directory", silent.local_addr().unwrap());
    acme.attempt_limit = Duration::from_millis(300);
    let (rustls, _addr, handle) = listener(&crate::tls_cert::stand_in(NAME).unwrap()).await;
    let status: StatusCell = Arc::default();
    let renewed = tokio::time::timeout(Duration::from_secs(20), acme.renew(&rustls, &status))
        .await
        .expect("the attempt is cut off");
    assert_eq!(renewed, Err(FIRST_RETRY));
    assert_eq!(
        status_of(&status).last_error.as_deref(),
        Some("the attempt did not end within 300ms")
    );
    assert!(!acme.files.cert.exists());
    drop(silent);
    handle.shutdown();
}

#[test]
fn the_status_records_failures_successes_and_the_stored_end() {
    let status: StatusCell = Arc::default();
    record_expires(&status, Some(5));
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(5),
            last_error: None,
            failures: 0,
        }
    );
    assert_eq!(record_failure(&status, "a".into()), (1, false));
    assert_eq!(record_failure(&status, "a".into()), (2, true));
    assert_eq!(record_failure(&status, "b".into()), (3, false));
    record_expires(&status, None);
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: None,
            last_error: Some("b".into()),
            failures: 3,
        }
    );
    record_ok(&status, 9);
    assert_eq!(
        status_of(&status),
        CertStatus {
            expires: Some(9),
            last_error: None,
            failures: 0,
        }
    );
}

#[test]
fn failed_attempts_are_retried_after_a_minute_doubling_to_six_hours() {
    let min = |m: u64| Duration::from_secs(60 * m);
    assert_eq!(retry_delay(0), min(1));
    assert_eq!(retry_delay(1), min(1));
    assert_eq!(retry_delay(2), min(2));
    assert_eq!(retry_delay(3), min(4));
    assert_eq!(retry_delay(9), min(256));
    assert_eq!(retry_delay(10), min(360));
    assert_eq!(retry_delay(u32::MAX), min(360));
    assert_eq!(FIRST_RETRY, min(1));
    assert_eq!(MAX_RETRY, min(360));
    assert_eq!(CHECK_EVERY, min(720));
}

#[test]
fn an_attempt_is_cut_off_far_beyond_a_slow_one_that_works() {
    assert_eq!(CA_CHECK, Duration::from_secs(120));
    assert_eq!(ATTEMPT_LIMIT, Duration::from_secs(1800));
    let slowest = Duration::from_secs(iem_core::config::MAX_PROPAGATION_S)
        + 2 * CA_CHECK
        + 10 * crate::cloudflare::TIMEOUT;
    assert!(ATTEMPT_LIMIT > slowest, "{slowest:?}");
    assert_eq!(
        cut_off(ATTEMPT_LIMIT),
        "the attempt did not end within 1800s"
    );
}

#[test]
fn a_missing_token_is_looked_for_every_five_minutes_and_warned_of_once() {
    assert_eq!(NO_TOKEN_RETRY, Duration::from_secs(300));
    assert_eq!(next_attempt(NO_TOKEN, 1), NO_TOKEN_RETRY);
    assert_eq!(next_attempt(NO_TOKEN, 12), NO_TOKEN_RETRY);
    assert_eq!(next_attempt("HTTP 500", 1), FIRST_RETRY);
    assert_eq!(next_attempt("HTTP 500", 12), MAX_RETRY);
    assert!(warns(NO_TOKEN, false));
    assert!(!warns(NO_TOKEN, true));
    assert!(warns("HTTP 500", true));
    assert!(warns("HTTP 500", false));
}

#[test]
fn a_stored_account_counts_for_its_own_directory_only() {
    let directory = "https://ca.example/directory";
    let bytes = serde_json::to_vec(&serde_json::json!({
        "directory": directory,
        "credentials": {
            "id": "https://ca.example/acct/1",
            "key_pkcs8": "AAAA",
            "directory": directory,
        },
    }))
    .unwrap();
    assert!(stored_account(&bytes, directory).is_some());
    assert!(stored_account(&bytes, "https://other.example/directory").is_none());
    assert!(stored_account(b"{not json", directory).is_none());
    // The key in a file of another shape never reaches the log.
    let quoted = "QUOTEDVALUE";
    let odd = format!(r#"{{"directory": "{directory}", "credentials": "{quoted}"}}"#);
    let error = serde_json::from_str::<StoredAccount>(&odd)
        .err()
        .expect("a string is no account");
    assert!(
        error.to_string().contains(quoted),
        "serde quotes it: {error}"
    );
    let problem = parse_problem(&error);
    assert!(!problem.contains(quoted), "{problem}");
    assert!(
        problem.starts_with("Data error at line 1 column "),
        "{problem}"
    );
}

#[test]
fn a_certificate_is_kept_until_thirty_days_are_left() {
    let now = 1_000_000_000;
    let pem = |days: i64| {
        Stored::Serve(Box::new(Pem {
            chain: String::new(),
            key: String::new(),
            info: CertInfo {
                names: vec![NAME.into()],
                not_after: now + days * DAY,
            },
        }))
    };
    assert_eq!(plan(&pem(60), now), Plan::Keep);
    assert_eq!(plan(&pem(30), now), Plan::Keep);
    assert_eq!(plan(&pem(29), now), Plan::Obtain);
    assert_eq!(plan(&Stored::Missing, now), Plan::Obtain);
    assert_eq!(plan(&Stored::Unusable("x".into()), now), Plan::Obtain);
}

#[test]
fn unreadable_files_are_unusable_not_missing() {
    let dir = tempfile::tempdir().unwrap();
    let files = CertFiles::new(dir.path(), "cert.pem", "key.pem");
    assert!(matches!(stored(&files, NAME), Stored::Missing));
    std::fs::create_dir(&files.cert).unwrap();
    match stored(&files, NAME) {
        Stored::Unusable(why) => {
            assert!(why.starts_with("reading the certificate files: "), "{why}")
        }
        other => panic!("{other:?}"),
    }
}
