//! The LAN 443 identity (#9 2026-09-28): the parsers, and the check against
//! a real rustls server on loopback, TLS 1.3 and 1.2, with Ed25519
//! certificates built here (no key material in the repository).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::crypto::ring as provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::version::{TLS12, TLS13};
use rustls::{ServerConfig, ServerConnection, StreamOwned, SupportedProtocolVersion};

use super::*;
use crate::bundle::sha256_hex;
use crate::cancel::Cancel;
use crate::pc::StepError;

const HOST: &str = "mixer.example.org";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// 2026-09-28 12:34:56 UTC.
const NOW: i64 = 1_790_598_896;
const VERSIONS: [&SupportedProtocolVersion; 2] = [&TLS13, &TLS12];
/// A validity around [`NOW`]: a UTCTime and a GeneralizedTime.
const FROM: &str = "200101000000Z";
const UNTIL: &str = "20500101000000Z";
/// What the check sends, once the certificate is the server's own.
const ASKED: &str = "GET /api/version HTTP/1.0\r\nHost: mixer.example.org\r\n\
                     Accept: application/json\r\nUser-Agent: iemmixer-guard\r\n\r\n";
const BAD_TIME: &str = "is not a certificate time (YYMMDDHHMMSSZ or YYYYMMDDHHMMSSZ)";

// ---- certificates, built here ----

/// One DER element (definite length, up to 64 KiB).
fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let n = content.len();
    let mut out = vec![tag];
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend([0x81, n as u8]);
    } else {
        out.extend([0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

/// Ed25519's AlgorithmIdentifier (RFC 8410): its OID 1.3.101.112 alone.
fn ed25519() -> Vec<u8> {
    seq(&[der(0x06, &[0x2b, 0x65, 0x70])])
}

/// A BIT STRING without unused bits.
fn bits(bytes: &[u8]) -> Vec<u8> {
    der(0x03, &[&[0][..], bytes].concat())
}

/// A self-signed Ed25519 certificate and its key.
struct Made {
    der: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

/// A certificate valid from `not_before` to `not_after`: 13 characters are
/// a UTCTime, any other length a GeneralizedTime. `v3` writes the version
/// field (a v1 certificate has none).
fn made(not_before: &str, not_after: &str, v3: bool) -> Made {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let time = |t: &str| der(if t.len() == 13 { 0x17 } else { 0x18 }, t.as_bytes());
    let cn = seq(&[der(0x06, &[0x55, 0x04, 0x03]), der(0x0c, b"iemmixer test")]);
    let name = seq(&[der(0x31, &cn)]);
    let spki = seq(&[ed25519(), bits(pair.public_key().as_ref())]);
    let mut fields = Vec::new();
    if v3 {
        fields.push(der(0xa0, &der(0x02, &[2])));
    }
    fields.extend([
        der(0x02, &[1]),
        ed25519(),
        name.clone(),
        seq(&[time(not_before), time(not_after)]),
        name,
        spki,
    ]);
    let tbs = seq(&fields);
    let signature = pair.sign(&tbs);
    let cert = seq(&[tbs, ed25519(), bits(signature.as_ref())]);
    Made {
        der: CertificateDer::from(cert),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_ref().to_vec())),
    }
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A PEM file of `certs`, in order.
fn pem(certs: &[&CertificateDer<'_>]) -> String {
    certs
        .iter()
        .map(|c| {
            let text = base64(c.as_ref());
            let lines: Vec<&str> = text
                .as_bytes()
                .chunks(64)
                .map(|l| std::str::from_utf8(l).unwrap())
                .collect();
            format!(
                "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
                lines.join("\n")
            )
        })
        .collect()
}

fn cert_file(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("cert.pem");
    std::fs::write(&path, text).unwrap();
    path
}

// ---- a server on loopback ----

/// One connection's TLS server: it presents `chain` and signs with `key`
/// (another key than the certificate's fails the handshake), reads one
/// request and answers `reply` after `delay`. `seen` gets the SNI and the
/// request ("" when the client sent none).
struct Server {
    addr: SocketAddr,
    seen: mpsc::Receiver<(Option<String>, String)>,
}

fn serve(
    chain: Vec<CertificateDer<'static>>,
    key: &PrivateKeyDer<'static>,
    version: &'static SupportedProtocolVersion,
    reply: Vec<u8>,
    delay: Duration,
) -> Server {
    let signer = provider::sign::any_supported_type(key).unwrap();
    let resolver = SingleCertAndKey::from(CertifiedKey::new(chain, signer));
    let config = ServerConfig::builder_with_provider(Arc::new(provider::default_provider()))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, seen) = mpsc::channel();
    thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn = ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = StreamOwned::new(conn, sock);
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match tls.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let sni = tls.conn.server_name().map(str::to_owned);
        tx.send((sni, String::from_utf8_lossy(&request).into_owned()))
            .unwrap();
        if !request.is_empty() {
            thread::sleep(delay);
            tls.write_all(&reply).unwrap();
            tls.conn.send_close_notify();
            tls.flush().unwrap();
        }
    });
    Server { addr, seen }
}

impl Server {
    fn seen(&self) -> (Option<String>, String) {
        self.seen.recv_timeout(Duration::from_secs(5)).unwrap()
    }
}

/// `/api/version` as iem-server answers it.
fn version_reply(hash: &str) -> Vec<u8> {
    let body = format!(r#"{{"version":"2.0.0-dev.9","git_hash":"{hash}"}}"#);
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn lan(addr: SocketAddr, cert: &Path) -> Lan<'_> {
    Lan {
        addr,
        host: HOST,
        cert,
        sha: SHA,
        limit: Duration::from_secs(3),
    }
}

/// The check of a server presenting `served` (its own key) against a
/// `cert.pem` holding `ours`, answering `reply`.
fn check_against(
    ours: &Made,
    served: &Made,
    version: &'static SupportedProtocolVersion,
    reply: Vec<u8>,
) -> (R<Option<String>>, (Option<String>, String)) {
    let dir = tempfile::tempdir().unwrap();
    let path = cert_file(dir.path(), &pem(&[&ours.der]));
    let server = serve(
        vec![served.der.clone()],
        &served.key,
        version,
        reply,
        Duration::ZERO,
    );
    let r = check(&lan(server.addr, &path), NOW, &Cancel::default());
    (r, server.seen())
}

// ---- the check ----

#[test]
fn the_servers_own_certificate_passes_and_the_version_is_asked() {
    for version in VERSIONS {
        let cert = made(FROM, UNTIL, true);
        let (r, (sni, asked)) = check_against(&cert, &cert, version, version_reply("0123456"));
        assert_eq!(r, Ok(None), "{version:?}");
        // SNI names the public host; the request goes to it.
        assert_eq!(sni.as_deref(), Some(HOST), "{version:?}");
        assert_eq!(asked, ASKED, "{version:?}");
    }
}

/// The certificate the server took over from the predecessor expired, and
/// the predecessor serves the same one (the PC, #9 2026-09-28): identity
/// holds, the validity is only named.
#[test]
fn a_certificate_outside_its_validity_passes_and_is_named() {
    for version in VERSIONS {
        let cert = made(FROM, "210101000000Z", true);
        let (r, (_, asked)) = check_against(&cert, &cert, version, version_reply("0123456"));
        assert_eq!(
            r,
            Ok(Some(
                "the LAN certificate expired on 2021-01-01; the predecessor serves the same one"
                    .to_owned()
            )),
            "{version:?}"
        );
        assert_eq!(asked, ASKED);
    }
    let cert = made("20400101000000Z", UNTIL, true);
    let (r, _) = check_against(&cert, &cert, &TLS13, version_reply("0123456"));
    assert_eq!(
        r,
        Ok(Some(
            "the LAN certificate is valid only from 2040-01-01; the predecessor serves the same one"
                .to_owned()
        ))
    );
    // A validity this parser cannot read is named too, never a failure: the
    // certificate is the server's own.
    let cert = made(FROM, "garbage", true);
    let (r, _) = check_against(&cert, &cert, &TLS13, version_reply("0123456"));
    assert_eq!(
        r,
        Ok(Some(format!(
            "the LAN certificate's validity is unreadable (\"garbage\" {BAD_TIME})"
        )))
    );
}

#[test]
fn another_certificate_fails_and_is_asked_nothing() {
    for version in VERSIONS {
        let (ours, other) = (made(FROM, UNTIL, true), made(FROM, UNTIL, true));
        let (r, (_, asked)) = check_against(&ours, &other, version, version_reply("0123456"));
        assert_eq!(
            r,
            Err(StepError::Failed(format!(
                "serves another certificate (SHA-256 {}), not the server's own (SHA-256 {})",
                sha256_hex(&other.der),
                sha256_hex(&ours.der)
            ))),
            "{version:?}"
        );
        assert_eq!(
            asked, "",
            "{version:?}: nothing goes to another certificate"
        );
    }
}

/// The pin is real: our certificate presented without its key fails the
/// handshake (its signature), before anything is sent.
#[test]
fn the_certificate_without_its_key_fails_the_handshake() {
    for version in VERSIONS {
        let (ours, other) = (made(FROM, UNTIL, true), made(FROM, UNTIL, true));
        let dir = tempfile::tempdir().unwrap();
        let path = cert_file(dir.path(), &pem(&[&ours.der]));
        let server = serve(
            vec![ours.der.clone()],
            &other.key,
            version,
            version_reply("0123456"),
            Duration::ZERO,
        );
        match check(&lan(server.addr, &path), NOW, &Cancel::default()) {
            Err(StepError::Failed(why)) => {
                assert!(why.starts_with("the TLS handshake: "), "{version:?}: {why}");
            }
            other => panic!("{version:?}: {other:?}"),
        }
        assert_eq!(server.seen().1, "", "{version:?}");
    }
}

#[test]
fn the_version_must_name_the_bundle() {
    let cert = made(FROM, UNTIL, true);
    let (r, _) = check_against(&cert, &cert, &TLS13, version_reply("89abcde"));
    assert_eq!(
        r,
        Err(StepError::Failed(format!(
            "/api/version names \"89abcde\", not {SHA}"
        )))
    );
    let reply = b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n".to_vec();
    let (r, _) = check_against(&cert, &cert, &TLS13, reply);
    assert_eq!(
        r,
        Err(StepError::Failed("/api/version: HTTP 503".to_owned()))
    );
}

#[test]
fn a_slow_answer_is_waited_for() {
    let cert = made(FROM, UNTIL, true);
    let dir = tempfile::tempdir().unwrap();
    let path = cert_file(dir.path(), &pem(&[&cert.der]));
    let server = serve(
        vec![cert.der.clone()],
        &cert.key,
        &TLS13,
        version_reply("0123456"),
        Duration::from_millis(400),
    );
    assert_eq!(
        check(&lan(server.addr, &path), NOW, &Cancel::default()),
        Ok(None)
    );
}

/// A server that takes the connection and never answers: the check ends
/// at its limit, not later.
#[test]
fn a_server_that_never_answers_fails_within_the_limit() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = cert_file(dir.path(), &pem(&[&made(FROM, UNTIL, true).der]));
    let (tx, rx) = mpsc::channel();
    let start = Instant::now();
    thread::spawn(move || {
        let lan = Lan {
            limit: Duration::from_millis(300),
            ..lan(addr, &path)
        };
        tx.send(check(&lan, NOW, &Cancel::default())).unwrap();
    });
    let r = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("the check ends at its limit");
    assert_eq!(
        r,
        Err(StepError::Failed("no answer within 300 ms".to_owned()))
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    drop(silent);
}

/// "ide event" ends the wait within a second (design §5.2).
#[test]
fn ide_event_ends_the_wait() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = cert_file(dir.path(), &pem(&[&made(FROM, UNTIL, true).der]));
    let c = Cancel::default();
    let later = c.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        later.preempt();
    });
    let start = Instant::now();
    let lan = Lan {
        limit: Duration::from_secs(5),
        ..lan(silent.local_addr().unwrap(), &path)
    };
    assert_eq!(check(&lan, NOW, &c), Err(StepError::Preempted));
    assert!(
        start.elapsed() < Duration::from_millis(1200),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_check_after_ide_event_connects_to_nothing() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    silent.set_nonblocking(true).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = cert_file(dir.path(), &pem(&[&made(FROM, UNTIL, true).der]));
    let c = Cancel::default();
    c.preempt();
    assert_eq!(
        check(&lan(silent.local_addr().unwrap(), &path), NOW, &c),
        Err(StepError::Preempted)
    );
    thread::sleep(Duration::from_millis(200));
    assert!(
        silent
            .accept()
            .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
    );
}

#[test]
fn the_certificate_file_must_hold_a_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let nowhere = SocketAddr::from(([127, 0, 0, 1], 9));
    let missing = dir.path().join("cert.pem");
    match check(&lan(nowhere, &missing), NOW, &Cancel::default()) {
        Err(StepError::Failed(why)) => {
            assert!(
                why.starts_with(&format!("{}: ", missing.display())),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    let path = cert_file(dir.path(), "not a certificate\n");
    assert_eq!(
        check(&lan(nowhere, &path), NOW, &Cancel::default()),
        Err(StepError::Failed(format!(
            "{}: no PEM certificate (no items found)",
            path.display()
        )))
    );
}

// ---- the parts ----

#[test]
fn the_leaf_is_the_first_certificate_of_the_file() {
    let (a, b) = (made(FROM, UNTIL, true), made(FROM, UNTIL, true));
    assert_eq!(leaf(pem(&[&a.der, &b.der]).as_bytes()), Ok(a.der.clone()));
    assert_eq!(leaf(pem(&[&b.der]).as_bytes()), Ok(b.der.clone()));
    assert_eq!(
        leaf(b"not a certificate\n"),
        Err("no PEM certificate (no items found)".to_owned())
    );
}

#[test]
fn certificates_are_the_same_only_byte_for_byte() {
    let (a, b) = (made(FROM, UNTIL, true), made(FROM, UNTIL, true));
    assert_eq!(same(&a.der, &a.der.clone()), Ok(()));
    assert_eq!(
        same(&a.der, &b.der),
        Err(format!(
            "serves another certificate (SHA-256 {}), not the server's own (SHA-256 {})",
            sha256_hex(&a.der),
            sha256_hex(&b.der)
        ))
    );
}

#[test]
fn the_request_names_the_host() {
    assert_eq!(request(HOST, "/api/version"), ASKED);
}

#[test]
fn a_der_element_has_a_short_or_a_long_length() {
    assert_eq!(
        element(&[0x04, 0x02, 1, 2, 9]),
        Ok((0x04, &[1u8, 2][..], &[9u8][..]))
    );
    let short = [&[0x04, 0x7f][..], &[7; 0x7f]].concat();
    assert_eq!(element(&short), Ok((0x04, &[7u8; 0x7f][..], &[][..])));
    let long = [&[0x04, 0x81, 0x80][..], &[7; 0x80], &[9]].concat();
    assert_eq!(element(&long), Ok((0x04, &[7u8; 0x80][..], &[9u8][..])));
    let two = [&[0x04, 0x82, 0x01, 0x02][..], &[7; 0x102]].concat();
    assert_eq!(
        element(&two).map(|(t, c, r)| (t, c.len(), r.len())),
        Ok((0x04, 0x102, 0))
    );
    assert_eq!(
        element(&[0x04, 0x84, 0, 0, 0, 1, 7]),
        Ok((0x04, &[7u8][..], &[][..]))
    );
    for truncated in [
        &[][..],
        &[0x04],
        &[0x04, 0x81],
        &[0x04, 0x82, 0x01],
        &[0x04, 0x03, 1, 2],
    ] {
        assert_eq!(
            element(truncated),
            Err("a truncated DER element".to_owned()),
            "{truncated:?}"
        );
    }
    // BER's indefinite length, and lengths beyond four bytes.
    assert_eq!(
        element(&[0x04, 0x80, 1]),
        Err("a DER length of 0 bytes".to_owned())
    );
    assert_eq!(
        element(&[0x04, 0x85, 0, 0, 0, 0, 1, 7]),
        Err("a DER length of 5 bytes".to_owned())
    );
}

#[test]
fn certificate_times_are_read_in_both_forms() {
    let t = |tag, text: &str| time(tag, text.as_bytes());
    let ok = |unix, day: &str| {
        Ok(Time {
            unix,
            day: day.to_owned(),
        })
    };
    assert_eq!(t(UTC_TIME, "260928123456Z"), ok(NOW, "2026-09-28"));
    assert_eq!(t(UTC_TIME, "700101000000Z"), ok(0, "1970-01-01"));
    assert_eq!(t(UTC_TIME, "700301000000Z"), ok(5_097_600, "1970-03-01"));
    assert_eq!(t(UTC_TIME, "000229120000Z"), ok(951_825_600, "2000-02-29"));
    assert_eq!(
        t(UTC_TIME, "241231235959Z"),
        ok(1_735_689_599, "2024-12-31")
    );
    // A UTCTime's 50–99 are 19xx, 00–49 20xx (RFC 5280 §4.1.2.5.1).
    assert_eq!(
        t(UTC_TIME, "491231235959Z"),
        ok(2_524_607_999, "2049-12-31")
    );
    assert_eq!(t(UTC_TIME, "500101000000Z"), ok(-631_152_000, "1950-01-01"));
    assert_eq!(
        t(GENERALIZED_TIME, "20500101000000Z"),
        ok(2_524_608_000, "2050-01-01")
    );
    assert_eq!(
        t(GENERALIZED_TIME, "21000301000000Z"),
        ok(4_107_542_400, "2100-03-01")
    );
    assert_eq!(
        t(GENERALIZED_TIME, "99991231235959Z"),
        ok(253_402_300_799, "9999-12-31")
    );
    assert_eq!(
        t(GENERALIZED_TIME, "19500101000000Z"),
        ok(-631_152_000, "1950-01-01")
    );
    assert_eq!(
        t(GENERALIZED_TIME, "19491231235959Z"),
        Err("\"19491231235959Z\" is before 1950".to_owned())
    );
    for bad in [
        "",
        "2",
        "260928123456",
        "2609281234Z",
        "260928123456ZZ",
        "2609a8123456Z",
        "260028123456Z",
        "261328123456Z",
        "260900123456Z",
        "260932123456Z",
        "260928243456Z",
        "260928126056Z",
        "260928123460Z",
    ] {
        assert_eq!(
            t(UTC_TIME, bad),
            Err(format!("{bad:?} {BAD_TIME}")),
            "{bad}"
        );
    }
    for bad in [
        "202",
        "2026092812345Z",
        "20260928123456",
        "2o260928123456Z",
        "260928123456Z",
    ] {
        assert_eq!(
            t(GENERALIZED_TIME, bad),
            Err(format!("{bad:?} {BAD_TIME}")),
            "{bad}"
        );
    }
    assert_eq!(
        t(0x04, "260928123456Z"),
        Err("a time of DER tag 0x04".to_owned())
    );
}

#[test]
fn the_validity_is_read_from_the_certificate() {
    let expected = Validity {
        not_before: Time {
            unix: 1_577_836_800,
            day: "2020-01-01".to_owned(),
        },
        not_after: Time {
            unix: 2_524_608_000,
            day: "2050-01-01".to_owned(),
        },
    };
    assert_eq!(validity(&made(FROM, UNTIL, true).der), Ok(expected.clone()));
    // A v1 certificate has no version field.
    assert_eq!(validity(&made(FROM, UNTIL, false).der), Ok(expected));
    assert_eq!(
        validity(&[0x04, 0x00]),
        Err("the certificate: DER tag 0x04, not 0x30".to_owned())
    );
    assert_eq!(
        validity(&seq(&[der(0x04, &[])])),
        Err("its TBSCertificate: DER tag 0x04, not 0x30".to_owned())
    );
    let no_validity = seq(&[seq(&[der(0x02, &[1]), ed25519(), seq(&[]), der(0x04, &[])])]);
    assert_eq!(
        validity(&no_validity),
        Err("its validity: DER tag 0x04, not 0x30".to_owned())
    );
    let cert = made(FROM, UNTIL, true);
    assert_eq!(
        validity(&cert.der[..40]),
        Err("a truncated DER element".to_owned())
    );
}

#[test]
fn a_certificate_outside_its_validity_is_named() {
    let v = Validity {
        not_before: Time {
            unix: 100,
            day: "2020-01-01".to_owned(),
        },
        not_after: Time {
            unix: 200,
            day: "2021-01-01".to_owned(),
        },
    };
    for now in [100, 150, 200] {
        assert_eq!(note(&v, now), None, "{now}");
    }
    assert_eq!(
        note(&v, 201),
        Some(
            "the LAN certificate expired on 2021-01-01; the predecessor serves the same one"
                .to_owned()
        )
    );
    assert_eq!(
        note(&v, 99),
        Some(
            "the LAN certificate is valid only from 2020-01-01; the predecessor serves the same one"
                .to_owned()
        )
    );
}

#[test]
fn an_answer_is_read_once_complete() {
    let a = |raw: &str, ended| answer(raw.as_bytes(), ended);
    let some = |status, body: &str| Ok(Some((status, body.to_owned())));
    // By its length, before the server closes; bytes after it are not the
    // body.
    let two = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    assert_eq!(a(two, false), some(200, "{}"));
    assert_eq!(
        a("HTTP/1.0 200 OK\r\ncontent-length:2\r\n\r\n{}xx", false),
        some(200, "{}")
    );
    let short = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}";
    assert_eq!(a(short, false), Ok(None));
    assert_eq!(
        a(short, true),
        Err("the answer ended after 2 of 3 bytes".to_owned())
    );
    // Without a length: what came until the server closed.
    let busy = "HTTP/1.0 503 Service Unavailable\r\nServer: x\r\n\r\nbusy";
    assert_eq!(a(busy, false), Ok(None));
    assert_eq!(a(busy, true), some(503, "busy"));
    assert_eq!(a("HTTP/1.1 204\r\n\r\n", true), some(204, ""));
    // Headers not complete yet.
    let head = "HTTP/1.1 200 OK\r\nContent-Le";
    assert_eq!(a(head, false), Ok(None));
    for ended in [head, ""] {
        assert_eq!(
            a(ended, true),
            Err("the answer ended before its headers".to_owned())
        );
    }
    for (raw, why) in [
        (
            "HTTP/2 200 OK\r\n\r\n",
            r#"the status line "HTTP/2 200 OK""#,
        ),
        (
            "HTTP/1.1 20 OK\r\n\r\n",
            r#"the status line "HTTP/1.1 20 OK""#,
        ),
        (
            "HTTP/1.1 2000 OK\r\n\r\n",
            r#"the status line "HTTP/1.1 2000 OK""#,
        ),
        (
            "HTTP/1.1 2x0 OK\r\n\r\n",
            r#"the status line "HTTP/1.1 2x0 OK""#,
        ),
        ("HTTP/1.1\r\n\r\n", r#"the status line "HTTP/1.1""#),
        (
            "HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
            r#"the header line "no colon""#,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: two\r\n\r\n",
            r#"the content-length "two""#,
        ),
        (
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n",
            r#"the transfer-encoding "chunked" (an HTTP/1.0 answer has none)"#,
        ),
    ] {
        assert_eq!(a(raw, true), Err(why.to_owned()), "{raw:?}");
    }
    assert_eq!(
        answer(b"HTTP/1.1 200 OK\r\n\r\n\xff", true),
        Err("the answer's body is not UTF-8".to_owned())
    );
    assert_eq!(
        answer(b"HTTP/1.1 200 OK\r\nX: \xff\r\n\r\n", true),
        Err("the answer's headers are not UTF-8".to_owned())
    );
    // At most MAX_ANSWER bytes.
    assert_eq!(answer(&vec![b'x'; MAX_ANSWER], false), Ok(None));
    assert_eq!(
        answer(&vec![b'x'; MAX_ANSWER + 1], false),
        Err(format!("an answer of more than {MAX_ANSWER} bytes"))
    );
}

#[test]
fn read_chunk_classifies_a_read_of_the_answer() {
    let limit = Duration::from_secs(3);
    // A zero-length read is a clean end.
    let mut raw = Vec::new();
    assert_eq!(read_chunk(Ok(0), &[1, 2, 3], &mut raw, limit), Ok(true));
    assert!(raw.is_empty());
    // Bytes read append the first `n` of the buffer and continue.
    let mut raw = Vec::new();
    assert_eq!(read_chunk(Ok(2), b"hix", &mut raw, limit), Ok(false));
    assert_eq!(raw, b"hi");
    // A close without TLS's close_notify (UnexpectedEof) ends the answer.
    let eof = io::Error::from(io::ErrorKind::UnexpectedEof);
    let mut raw = Vec::new();
    assert_eq!(read_chunk(Err(eof), &[], &mut raw, limit), Ok(true));
    // Any other read error is fatal and passed through as an error.
    let reset = io::Error::from(io::ErrorKind::ConnectionReset);
    let mut raw = Vec::new();
    assert!(read_chunk(Err(reset), &[], &mut raw, limit).is_err());
}
