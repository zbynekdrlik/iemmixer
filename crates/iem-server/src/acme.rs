// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/acme.rs (iemmixer#91).
//! The certificate of `https_domain` from an ACME CA (#91): Let's Encrypt
//! by DNS-01 through the Cloudflare DNS API, in the server itself (nothing
//! at runtime depends on another machine).
//!
//! [`keep`] runs for the server's life when the site has `[acme]`: a stored
//! certificate (`tls_cert`/`tls_key`) with 30 days or more left is kept
//! (checked every [`CHECK_EVERY`]); otherwise one attempt orders a new one,
//! removes this server's own leftover challenge records, writes the
//! challenge record, lets it reach Cloudflare's name servers (`[acme]
//! propagation_s`), answers the challenge, removes the record (always, also
//! after a failure), and is cut off at [`ATTEMPT_LIMIT`]. The new pair is
//! written to the certificate files (each replaced whole) and the HTTPS
//! listener reloads them (`RustlsConfig::reload_from_pem_file`): no restart.
//! A failed attempt is logged with its reason, shown in `GET /api/site`
//! (`certificate`: `expires`, `last_error`, `failures`), and retried after
//! [`next_attempt`] (1 min doubling to 6 h; every 5 min while no Cloudflare
//! token is set, warned of once); the old certificate keeps being served
//! meanwhile.
//!
//! The ACME account's key is kept for the server's user (`sealed.rs`:
//! `secrets/acme_account.dpapi` on Windows, an owner-only
//! `secrets/acme_account.private` on the box); the Cloudflare token is
//! `cf_token.rs`'s. Neither is ever logged.
//!
//! Adapted from fohmixer: one name (no second keeper, so no shared turn),
//! the listener is axum-server's `RustlsConfig`, the Cloudflare calls go
//! through reqwest.

use std::path::PathBuf;
use std::time::Duration;

use axum_server::tls_rustls::RustlsConfig;
use iem_core::config::AcmeConfig;
use iem_core::tunnel::CertStatus;
use instant_acme::{
    Account, AccountBuilder, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier,
    NewAccount, NewOrder, Order, OrderStatus, RetryPolicy,
};
use serde::{Deserialize, Serialize};

use crate::cloudflare::Dns;
use crate::secrets::SECRETS_DIR;
use crate::tls_cert::{CertFiles, Pem};

/// How often a kept certificate is looked at again.
pub const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// The wait after a first failed attempt.
pub const FIRST_RETRY: Duration = Duration::from_secs(60);
/// The longest wait between two failed attempts.
pub const MAX_RETRY: Duration = Duration::from_secs(6 * 60 * 60);
/// The wait while no Cloudflare token is set: the check is local (nothing
/// is asked of anyone), so a token set later is used within minutes.
pub const NO_TOKEN_RETRY: Duration = Duration::from_secs(5 * 60);
/// How long the CA's checks (the DNS record's, then the certificate's) are
/// each polled.
pub const CA_CHECK: Duration = Duration::from_secs(120);
/// The longest one attempt may take: far beyond a slow one that works (the
/// propagation wait, at most `MAX_PROPAGATION_S`, the two [`CA_CHECK`]s and
/// a few Cloudflare exchanges of `cloudflare::TIMEOUT` each). A CA that
/// never answers (instant-acme's requests have no time limit of their own)
/// ends the attempt there.
pub const ATTEMPT_LIMIT: Duration = Duration::from_secs(30 * 60);
/// The account file's name without its extension.
pub const ACCOUNT_STEM: &str = "acme_account";
/// Why nothing happens without a token.
pub const NO_TOKEN: &str = "no Cloudflare API token: run `iem-server cloudflare set-token` as the server's \
     user (a token with Zone > DNS > Edit on the name's zone)";

/// The keeper's status, read by `GET /api/site` (`None` without `[acme]`).
pub type StatusCell = crate::CertStatusCell;

/// The wait before the next attempt after `failures` failed ones in a row:
/// [`FIRST_RETRY`] doubling, at most [`MAX_RETRY`].
pub fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_RETRY.saturating_mul(1 << doublings).min(MAX_RETRY)
}

/// The wait before the next attempt after `failures` failed ones in a row,
/// the last because of `why`.
pub fn next_attempt(why: &str, failures: u32) -> Duration {
    if why == NO_TOKEN {
        NO_TOKEN_RETRY
    } else {
        retry_delay(failures)
    }
}

/// Whether a failure is logged as a warning: always, except the missing
/// token again (every 5 min; the first one was a warning and `/api/site`
/// keeps showing it).
pub fn warns(why: &str, repeated: bool) -> bool {
    !repeated || why != NO_TOKEN
}

/// Why an attempt cut off at `limit` failed.
fn cut_off(limit: Duration) -> String {
    format!("the attempt did not end within {limit:?}")
}

/// Records a certificate served until `not_after`: no error, no failures.
fn record_ok(status: &StatusCell, not_after: i64) {
    let mut status = status.write().unwrap_or_else(|e| e.into_inner());
    *status = Some(CertStatus {
        expires: Some(not_after),
        last_error: None,
        failures: 0,
    });
}

/// Records the stored certificate's end (`None`: none stored), keeping the
/// failures and the last error.
fn record_expires(status: &StatusCell, expires: Option<i64>) {
    let mut status = status.write().unwrap_or_else(|e| e.into_inner());
    status.get_or_insert_with(CertStatus::default).expires = expires;
}

/// Records a failed attempt: the failures in a row, and whether it failed
/// for the same reason as the one before.
fn record_failure(status: &StatusCell, why: String) -> (u32, bool) {
    let mut status = status.write().unwrap_or_else(|e| e.into_inner());
    let status = status.get_or_insert_with(CertStatus::default);
    let repeated = status.last_error.as_deref() == Some(why.as_str());
    status.failures = status.failures.saturating_add(1);
    status.last_error = Some(why);
    (status.failures, repeated)
}

/// The stored account when it belongs to `directory`. A file that does not
/// parse is logged ([`parse_problem`]) and replaced by a new account.
fn stored_account(bytes: &[u8], directory: &str) -> Option<StoredAccount> {
    match serde_json::from_slice::<StoredAccount>(bytes) {
        Ok(account) => (account.directory == directory).then_some(account),
        Err(e) => {
            let problem = parse_problem(&e);
            tracing::warn!("ACME: the stored account does not parse ({problem}); making a new one");
            None
        }
    }
}

/// What is logged of a stored account that does not parse: the kind and
/// place of the error, never serde's text (it can quote a value: the key).
fn parse_problem(e: &serde_json::Error) -> String {
    format!(
        "{:?} error at line {} column {}",
        e.classify(),
        e.line(),
        e.column()
    )
}

/// What is stored for the name.
#[derive(Debug)]
pub enum Stored {
    /// No certificate file, or no key file.
    Missing,
    /// Files that cannot be read or served for the name: why.
    Unusable(String),
    /// A checked pair.
    Serve(Box<Pem>),
}

/// What `files` hold for `name`.
pub fn stored(files: &CertFiles, name: &str) -> Stored {
    match files.load(name) {
        Ok(None) => Stored::Missing,
        Ok(Some(Ok(pem))) => Stored::Serve(Box::new(pem)),
        Ok(Some(Err(why))) => Stored::Unusable(why),
        Err(e) => Stored::Unusable(format!("reading the certificate files: {e}")),
    }
}

/// What the keeper does with the stored certificate at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// It has 30 days or more: keep it.
    Keep,
    /// None, unusable or due: get a new one.
    Obtain,
}

/// The plan for `stored` at `now`.
pub fn plan(stored: &Stored, now: i64) -> Plan {
    match stored {
        Stored::Serve(pem) if !pem.info.renewal_due(now) => Plan::Keep,
        _ => Plan::Obtain,
    }
}

/// The account on disk: the directory it belongs to and its credentials.
#[derive(Serialize, Deserialize)]
struct StoredAccount {
    directory: String,
    credentials: AccountCredentials,
}

/// The ACME client of the site's name.
pub struct Acme {
    pub name: String,
    pub cfg: AcmeConfig,
    /// The site's folder: `secrets/` is in it.
    pub config_dir: PathBuf,
    /// The certificate files the HTTPS listener serves.
    pub files: CertFiles,
    /// The Cloudflare API (`cloudflare::API`; a double in the tests).
    pub cloudflare_api: String,
    pub http: reqwest::Client,
    /// How long the CA's checks are polled.
    pub retry: RetryPolicy,
    /// Where an attempt is cut off ([`ATTEMPT_LIMIT`]; shorter in the tests).
    pub attempt_limit: Duration,
}

impl Acme {
    /// The client of `name` with `cfg`'s CA, the real Cloudflare API.
    pub fn new(
        name: &str,
        cfg: &AcmeConfig,
        config_dir: &std::path::Path,
        files: CertFiles,
    ) -> Result<Self, String> {
        Ok(Self {
            name: name.to_string(),
            cfg: cfg.clone(),
            config_dir: config_dir.to_path_buf(),
            files,
            cloudflare_api: crate::cloudflare::API.to_string(),
            http: crate::cloudflare::client()?,
            retry: RetryPolicy::new().timeout(CA_CHECK),
            attempt_limit: ATTEMPT_LIMIT,
        })
    }

    fn secrets(&self) -> PathBuf {
        self.config_dir.join(SECRETS_DIR)
    }

    fn account_path(&self) -> PathBuf {
        self.secrets()
            .join(format!("{ACCOUNT_STEM}.{}", crate::sealed::SEALED_EXT))
    }

    /// The HTTP client for the CA: the verified HTTPS default, plain http
    /// only for a loopback directory (`acme_url_allowed`: a test double).
    fn builder(&self) -> Result<AccountBuilder, String> {
        if self.cfg.directory.starts_with("http://") {
            let plain =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .build_http::<instant_acme::BodyWrapper<bytes::Bytes>>();
            return Ok(Account::builder_with_http(Box::new(plain)));
        }
        Account::builder().map_err(|e| format!("the ACME HTTP client: {e}"))
    }

    /// The account: the stored one of this directory, else a new one
    /// (stored at once).
    async fn account(&self) -> Result<Account, String> {
        let path = self.account_path();
        let stored = crate::sealed::read(&path)
            .map_err(|e| format!("reading the ACME account {}: {e}", path.display()))?
            .and_then(|bytes| stored_account(&bytes, &self.cfg.directory));
        if let Some(account) = stored {
            return self
                .builder()?
                .from_credentials(account.credentials)
                .await
                .map_err(|e| format!("the ACME account: {e}"));
        }
        let contact = self
            .cfg
            .contact
            .as_ref()
            .map(|address| format!("mailto:{address}"));
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, credentials) = self
            .builder()?
            .create(
                &NewAccount {
                    contact: &contacts,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.cfg.directory.clone(),
                None,
            )
            .await
            .map_err(|e| format!("creating the ACME account: {e}"))?;
        let record = StoredAccount {
            directory: self.cfg.directory.clone(),
            credentials,
        };
        let json = serde_json::to_vec(&record).map_err(|e| format!("the ACME account: {e}"))?;
        std::fs::create_dir_all(self.secrets())
            .and_then(|()| crate::sealed::write(&path, &json))
            .map_err(|e| format!("storing the ACME account {}: {e}", path.display()))?;
        tracing::info!(directory = %self.cfg.directory, "ACME: created an account");
        Ok(account)
    }

    /// Writes the DNS-01 record of every pending authorization and answers
    /// its challenge; the record ids go to `records` as they are made.
    async fn authorize(
        &self,
        order: &mut Order,
        dns: &Dns<'_>,
        zone: &str,
        records: &mut Vec<String>,
    ) -> Result<(), String> {
        let mut authorizations = order.authorizations();
        while let Some(next) = authorizations.next().await {
            let mut authz = next.map_err(|e| format!("ACME authorization: {e}"))?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => return Err(format!("ACME authorization is {other:?}")),
            }
            let mut challenge = authz
                .challenge(ChallengeType::Dns01)
                .ok_or("the CA offers no DNS-01 challenge")?;
            let fqdn = format!("_acme-challenge.{}", challenge.identifier());
            let value = challenge.key_authorization().dns_value();
            let id = dns.add_txt(zone, &fqdn, &value).await?;
            tracing::info!(record = %id, %fqdn, "ACME: challenge record written");
            records.push(id);
            tokio::time::sleep(Duration::from_secs(self.cfg.propagation_s)).await;
            challenge
                .set_ready()
                .await
                .map_err(|e| format!("ACME challenge: {e}"))?;
        }
        let status = order
            .poll_ready(&self.retry)
            .await
            .map_err(|e| format!("ACME check of the DNS record: {e}"))?;
        if status == OrderStatus::Ready {
            Ok(())
        } else {
            Err(format!(
                "the CA did not accept the DNS record (order {status:?})"
            ))
        }
    }

    /// One attempt: a new certificate for the name, checked.
    pub async fn issue(&self) -> Result<Pem, String> {
        let token = crate::cf_token::load(&self.secrets())
            .map_err(|e| e.to_string())?
            .ok_or(NO_TOKEN)?;
        let dns = Dns::new(&self.http, &self.cloudflare_api, &token);
        let zone = dns.zone_id(&self.name).await?;
        let fqdn = format!("_acme-challenge.{}", self.name);
        // A record an interrupted attempt left behind.
        for id in dns.txt_records(&zone, &fqdn).await? {
            dns.delete(&zone, &id).await?;
            tracing::info!(record = %id, "ACME: removed a challenge record an earlier attempt left");
        }
        let account = self.account().await?;
        let mut order = account
            .new_order(&NewOrder::new(&[Identifier::Dns(self.name.clone())]))
            .await
            .map_err(|e| format!("ACME order: {e}"))?;
        let mut records = Vec::new();
        let authorized = self.authorize(&mut order, &dns, &zone, &mut records).await;
        for id in &records {
            match dns.delete(&zone, id).await {
                Ok(()) => tracing::info!(record = %id, "ACME: challenge record removed"),
                Err(why) => {
                    tracing::warn!(record = %id, "ACME: the challenge record stays: {why}")
                }
            }
        }
        authorized?;
        let key = order
            .finalize()
            .await
            .map_err(|e| format!("ACME finalize: {e}"))?;
        let chain = order
            .poll_certificate(&self.retry)
            .await
            .map_err(|e| format!("ACME certificate: {e}"))?;
        crate::tls_cert::checked(chain, key, &self.name)
    }

    /// Stores `pem` and has the listener serve it.
    async fn serve(&self, pem: &Pem, rustls: &RustlsConfig) -> Result<(), String> {
        self.files
            .save(pem)
            .map_err(|e| format!("storing the certificate: {e}"))?;
        rustls
            .reload_from_pem_file(&self.files.cert, &self.files.key)
            .await
            .map_err(|e| format!("serving the new certificate: {e}"))
    }

    /// One attempt cut off at the limit, then the new certificate stored and
    /// served: `Err(the wait before the next one)` when it fails.
    pub async fn renew(&self, rustls: &RustlsConfig, status: &StatusCell) -> Result<(), Duration> {
        tracing::info!(name = %self.name, directory = %self.cfg.directory, "ACME: getting a certificate");
        // Only the order is cut off: a certificate it got is always stored
        // and served (a cut between the two would leave a stored one the
        // listener never loads). A challenge record a cut-off attempt
        // leaves goes at the next attempt.
        let attempt = match tokio::time::timeout(self.attempt_limit, self.issue()).await {
            Ok(issued) => issued,
            Err(_) => Err(cut_off(self.attempt_limit)),
        };
        let served = match attempt {
            Ok(pem) => self.serve(&pem, rustls).await.map(|()| pem),
            Err(why) => Err(why),
        };
        match served {
            Ok(pem) => {
                record_ok(status, pem.info.not_after);
                tracing::info!(name = %self.name, not_after = pem.info.not_after, "ACME: new certificate served");
                Ok(())
            }
            Err(why) => {
                let (failures, repeated) = record_failure(status, why.clone());
                let wait = next_attempt(&why, failures);
                if warns(&why, repeated) {
                    tracing::warn!(
                        name = %self.name,
                        failures,
                        retry_s = wait.as_secs(),
                        "ACME: no certificate: {why}"
                    );
                } else {
                    tracing::debug!(name = %self.name, failures, "ACME: still no certificate: {why}");
                }
                Err(wait)
            }
        }
    }

    /// One look at the stored certificate, and an attempt when it is due:
    /// the wait before the next look.
    pub async fn check(&self, rustls: &RustlsConfig, status: &StatusCell, now: i64) -> Duration {
        let stored = stored(&self.files, &self.name);
        match &stored {
            Stored::Serve(pem) => record_expires(status, Some(pem.info.not_after)),
            Stored::Missing => record_expires(status, None),
            Stored::Unusable(why) => {
                tracing::warn!(name = %self.name, "ACME: the stored certificate is not used: {why}");
                record_expires(status, None);
            }
        }
        match plan(&stored, now) {
            Plan::Keep => {
                if let Stored::Serve(pem) = &stored {
                    tracing::info!(
                        name = %self.name,
                        days_left = pem.info.days_left(now),
                        "ACME: the certificate is kept"
                    );
                }
                CHECK_EVERY
            }
            Plan::Obtain => match self.renew(rustls, status).await {
                Ok(()) => CHECK_EVERY,
                Err(wait) => wait,
            },
        }
    }
}

/// Keeps the name's certificate for the server's life (see the module doc).
pub async fn keep(acme: Acme, rustls: RustlsConfig, status: StatusCell) {
    loop {
        let now = chrono::Utc::now().timestamp();
        let wait = acme.check(&rustls, &status, now).await;
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
#[path = "acme/double.rs"]
pub(crate) mod double;

#[cfg(test)]
#[path = "acme/tests.rs"]
mod tests;
