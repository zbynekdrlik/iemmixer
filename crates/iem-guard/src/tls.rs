//! The LAN 443 identity of a dev/live entry (S6 design note §5.2 step 8,
//! §6; #9 2026-09-28): identity, not validity.
//!
//! iem-server serves LAN 443 with the certificate `iem-migrate band` took
//! over from the predecessor (the `cert.pem` next to the server's config),
//! and the predecessor serves the very same one: the band's phones know it,
//! expired or not (P9). So the check connects to this PC's port 443 with the
//! public host's name (SNI), accepts any chain and any validity, and
//! requires the served leaf to be byte-equal to that file's first
//! certificate; the handshake's own signature, verified with the leaf's
//! key, proves the server holds its key. Only then does it ask
//! `/api/version` (HTTP/1.0) for the bundle's SHA. A certificate outside its
//! validity is a note for the switch report and `iemmode status`, never a
//! failure. The public host stays a validated HTTPS check (curl, the
//! system's roots, Cloudflare's certificate).
//!
//! The client runs on a thread of its own, bounded by [`Lan::limit`]; "ide
//! event" returns the wait at once (the thread ends at its limit).

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::version::{TLS12, TLS13};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};

use crate::bundle::sha256_hex;
use crate::cancel::Cancel;
use crate::effects::web::{is_success, version_matches};
use crate::pc::{R, StepError};

/// This PC's port 443, where iem-server serves the LAN.
pub const LAN_443: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443);
/// How long one check may take (curl's bound for the public host).
pub const LIMIT: Duration = Duration::from_secs(10);
/// What the check asks.
const VERSION_PATH: &str = "/api/version";
/// The most of an answer read: `/api/version` is a few dozen bytes.
const MAX_ANSWER: usize = 65_536;

/// What LAN 443 must show.
#[derive(Debug, Clone, Copy)]
pub struct Lan<'a> {
    /// [`LAN_443`] on the PC.
    pub addr: SocketAddr,
    /// The public host: the SNI and the `Host` header.
    pub host: &'a str,
    /// The server's own certificate (`effects::web::server_cert`).
    pub cert: &'a Path,
    /// The bundle `/api/version` must name.
    pub sha: &'a str,
    /// [`LIMIT`] on the PC.
    pub limit: Duration,
}

/// LAN 443 serves the certificate of `lan.cert` (its first, byte-equal;
/// its key proven by the handshake) and `/api/version` names `lan.sha`.
/// `Ok(Some)`: a note on a certificate outside its validity at `now` (Unix
/// seconds), or one whose validity this parser cannot read.
pub fn check(lan: &Lan<'_>, now: i64, c: &Cancel) -> R<Option<String>> {
    let shown = lan.cert.display();
    let pem = fs::read(lan.cert).map_err(|e| StepError::failed(format!("{shown}: {e}")))?;
    let expected = leaf(&pem).map_err(|e| StepError::failed(format!("{shown}: {e}")))?;
    let (status, body) = fetch_within(lan, expected.clone(), c)?;
    if !is_success(status) {
        return Err(StepError::failed(format!("{VERSION_PATH}: HTTP {status}")));
    }
    version_matches(&body, lan.sha).map_err(StepError::Failed)?;
    Ok(match validity(&expected) {
        Ok(v) => note(&v, now),
        Err(e) => Some(format!(
            "the LAN certificate's validity is unreadable ({e})"
        )),
    })
}

/// The first certificate of a PEM file: the leaf the server presents.
pub fn leaf(pem: &[u8]) -> Result<CertificateDer<'static>, String> {
    CertificateDer::from_pem_slice(pem).map_err(|e| format!("no PEM certificate ({e})"))
}

/// The served certificate is the server's own, byte for byte.
pub fn same(served: &[u8], expected: &[u8]) -> Result<(), String> {
    if served == expected {
        Ok(())
    } else {
        Err(format!(
            "serves another certificate (SHA-256 {}), not the server's own (SHA-256 {})",
            sha256_hex(served),
            sha256_hex(expected)
        ))
    }
}

/// The GET the check sends: HTTP/1.0, so the answer is never chunked and
/// the server closes after it.
pub fn request(host: &str, path: &str) -> String {
    format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\n\
         User-Agent: iemmixer-guard\r\n\r\n"
    )
}

// ---- the validity ----

/// A certificate time: Unix seconds and its day (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Time {
    pub unix: i64,
    pub day: String,
}

/// A certificate's `notBefore` and `notAfter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validity {
    pub not_before: Time,
    pub not_after: Time,
}

/// Outside the validity at `now` (RFC 5280: valid from `notBefore` to
/// `notAfter`, both included): what the report and the status name.
pub fn note(v: &Validity, now: i64) -> Option<String> {
    if now > v.not_after.unix {
        Some(format!(
            "the LAN certificate expired on {}; the predecessor serves the same one",
            v.not_after.day
        ))
    } else if now < v.not_before.unix {
        Some(format!(
            "the LAN certificate is valid only from {}; the predecessor serves the same one",
            v.not_before.day
        ))
    } else {
        None
    }
}

const SEQUENCE: u8 = 0x30;
/// `[0] EXPLICIT Version` (absent in a v1 certificate).
const VERSION: u8 = 0xa0;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const TRUNCATED: &str = "a truncated DER element";

/// The validity of a DER certificate (RFC 5280 §4.1): Certificate →
/// TBSCertificate → [version], serialNumber, signature, issuer, validity.
pub fn validity(der: &[u8]) -> Result<Validity, String> {
    let (cert, _) = nested(der, SEQUENCE, "the certificate")?;
    let (tbs, _) = nested(cert, SEQUENCE, "its TBSCertificate")?;
    let (tag, _, after) = element(tbs)?;
    // After the version comes the serial number; without one `tag` was it.
    let rest = if tag == VERSION {
        element(after)?.2
    } else {
        after
    };
    let (_, _, rest) = element(rest)?; // signature
    let (_, _, rest) = element(rest)?; // issuer
    let (times, _) = nested(rest, SEQUENCE, "its validity")?;
    let (tag, before, rest) = element(times)?;
    let not_before = time(tag, before)?;
    let (tag, after, _) = element(rest)?;
    let not_after = time(tag, after)?;
    Ok(Validity {
        not_before,
        not_after,
    })
}

/// One DER element of `der`: its tag, its content and what follows it.
fn element(der: &[u8]) -> Result<(u8, &[u8], &[u8]), String> {
    let (&tag, rest) = der.split_first().ok_or(TRUNCATED)?;
    let (&first, rest) = rest.split_first().ok_or(TRUNCATED)?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let (bytes, rest) = rest
            .split_at_checked(usize::from(first & 0x7f))
            .ok_or(TRUNCATED)?;
        if bytes.is_empty() || bytes.len() > 4 {
            return Err(format!("a DER length of {} bytes", bytes.len()));
        }
        let len = bytes.iter().fold(0, |n, &b| (n << 8) | usize::from(b));
        (len, rest)
    };
    let (content, rest) = rest.split_at_checked(len).ok_or(TRUNCATED)?;
    Ok((tag, content, rest))
}

/// An element of `tag`: its content and what follows it.
fn nested<'a>(der: &'a [u8], tag: u8, what: &str) -> Result<(&'a [u8], &'a [u8]), String> {
    let (found, content, rest) = element(der)?;
    if found == tag {
        Ok((content, rest))
    } else {
        Err(format!("{what}: DER tag {found:#04x}, not {tag:#04x}"))
    }
}

/// A certificate time (RFC 5280 §4.1.2.5): a UTCTime `YYMMDDHHMMSSZ` (years
/// 1950–2049) or a GeneralizedTime `YYYYMMDDHHMMSSZ`.
fn time(tag: u8, text: &[u8]) -> Result<Time, String> {
    let shown = String::from_utf8_lossy(text);
    let bad = || format!("{shown:?} is not a certificate time (YYMMDDHHMMSSZ or YYYYMMDDHHMMSSZ)");
    let (year, rest) = match tag {
        UTC_TIME => {
            let (yy, rest) = text.split_at_checked(2).ok_or_else(bad)?;
            let yy = digits(yy).ok_or_else(bad)?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, rest)
        }
        GENERALIZED_TIME => {
            let (yyyy, rest) = text.split_at_checked(4).ok_or_else(bad)?;
            (digits(yyyy).ok_or_else(bad)?, rest)
        }
        other => return Err(format!("a time of DER tag {other:#04x}")),
    };
    let &[mo1, mo2, d1, d2, h1, h2, mi1, mi2, s1, s2, b'Z'] = rest else {
        return Err(bad());
    };
    let field = |a: u8, b: u8, lo: i64, hi: i64| {
        digits(&[a, b])
            .filter(|n| (lo..=hi).contains(n))
            .ok_or_else(bad)
    };
    let month = field(mo1, mo2, 1, 12)?;
    let day = field(d1, d2, 1, 31)?;
    let hour = field(h1, h2, 0, 23)?;
    let minute = field(mi1, mi2, 0, 59)?;
    let second = field(s1, s2, 0, 59)?;
    if year < 1950 {
        return Err(format!("{shown:?} is before 1950"));
    }
    Ok(Time {
        unix: days(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second,
        day: format!("{year:04}-{month:02}-{day:02}"),
    })
}

/// ASCII digits as a number.
fn digits(text: &[u8]) -> Option<i64> {
    text.iter().try_fold(0, |n, &b| {
        let digit = char::from(b).to_digit(10)?;
        Some(n * 10 + i64::from(digit))
    })
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar, year
/// ≥ 1 (Howard Hinnant's `days_from_civil`, eras of 400 years).
fn days(year: i64, month: i64, day: i64) -> i64 {
    // The year counted from March, so February's leap day ends it.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let year_of_era = y - era * 400;
    let march_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * march_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---- the connection ----

/// Any chain, any validity, any name: identity is the byte comparison right
/// after the handshake ([`same`]). The handshake's signatures are verified
/// with the leaf's key, so a server without that key never gets that far.
#[derive(Debug)]
struct SignaturesOnly(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for SignaturesOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// [`fetch`] on a thread of its own: its answer, or `Preempted` as soon as
/// "ide event" came (never a connection once it came). The thread ends at
/// its limit either way.
fn fetch_within(lan: &Lan<'_>, expected: CertificateDer<'static>, c: &Cancel) -> R<(u16, String)> {
    if c.preempted() {
        return Err(StepError::Preempted);
    }
    let (tx, rx) = mpsc::channel();
    let (addr, host, limit) = (lan.addr, lan.host.to_owned(), lan.limit);
    thread::Builder::new()
        .name("lan-443".to_owned())
        .spawn(move || {
            // A receiver that is gone was pre-empted: nobody waits for it.
            let _ = tx.send(fetch(addr, &host, &expected, limit));
        })
        .map_err(|e| StepError::failed(format!("the LAN 443 thread: {e}")))?;
    loop {
        match rx.recv_timeout(Cancel::SLICE) {
            Ok(answer) => return answer.map_err(StepError::Failed),
            Err(RecvTimeoutError::Timeout) => {
                if c.preempted() {
                    return Err(StepError::Preempted);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(StepError::failed(
                    "the LAN 443 check ended without an answer",
                ));
            }
        }
    }
}

/// TLS to `addr` with the SNI `host`; the served leaf must be `expected`
/// before anything is sent; then `/api/version`: its status and body.
/// Everything within `limit`.
fn fetch(
    addr: SocketAddr,
    host: &str,
    expected: &CertificateDer<'_>,
    limit: Duration,
) -> Result<(u16, String), String> {
    let deadline = Instant::now() + limit;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(SignaturesOnly(provider.signature_verification_algorithms));
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&TLS13, &TLS12])
        .map_err(|e| format!("the TLS configuration: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|e| format!("{host:?}: {e}"))?;
    let conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("the TLS connection: {e}"))?;
    let sock = TcpStream::connect_timeout(&addr, limit)
        .map_err(|e| io_error(&format!("connecting to {addr}"), &e, limit))?;
    let mut tls = StreamOwned::new(conn, sock);
    while tls.conn.is_handshaking() {
        left(&tls.sock, deadline, limit)?;
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| io_error("the TLS handshake", &e, limit))?;
    }
    let served = tls
        .conn
        .peer_certificates()
        .and_then(<[_]>::first)
        .ok_or("the handshake named no certificate")?;
    same(served, expected)?;
    left(&tls.sock, deadline, limit)?;
    tls.write_all(request(host, VERSION_PATH).as_bytes())
        .and_then(|()| tls.flush())
        .map_err(|e| io_error("the request", &e, limit))?;
    let mut raw = Vec::new();
    let mut buf = [0; 4096];
    loop {
        left(&tls.sock, deadline, limit)?;
        let read = tls.read(&mut buf);
        let ended = read_chunk(read, &buf, &mut raw, limit)?;
        if let Some(a) = answer(&raw, ended)? {
            return Ok(a);
        }
    }
}

/// Classifies one `read` of the answer: `Ok(true)` = the answer ended (a clean
/// close, or a close without TLS's close_notify), `Ok(false)` = more to read
/// (the `n` bytes are appended to `raw`), `Err` = a fatal read failure.
fn read_chunk(
    read: io::Result<usize>,
    buf: &[u8],
    raw: &mut Vec<u8>,
    limit: Duration,
) -> Result<bool, String> {
    match read {
        Ok(0) => Ok(true),
        Ok(n) => {
            raw.extend_from_slice(buf.get(..n).unwrap_or_default());
            Ok(false)
        }
        // A server that closes without TLS's close_notify.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
        Err(e) => Err(io_error("the answer", &e, limit)),
    }
}

/// The socket's timeouts set to what is left until `deadline`; none left
/// fails.
fn left(sock: &TcpStream, deadline: Instant, limit: Duration) -> Result<(), String> {
    let rest = deadline.saturating_duration_since(Instant::now());
    if rest.is_zero() {
        return Err(no_answer(limit));
    }
    sock.set_read_timeout(Some(rest))
        .and_then(|()| sock.set_write_timeout(Some(rest)))
        .map_err(|e| format!("the socket's timeouts: {e}"))
}

/// A timed-out read or write ends the check (Windows leaves such a socket
/// in an undefined state: it is never used again).
fn io_error(what: &str, e: &io::Error, limit: Duration) -> String {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => no_answer(limit),
        _ => format!("{what}: {e}"),
    }
}

fn no_answer(limit: Duration) -> String {
    format!("no answer within {} ms", limit.as_millis())
}

/// An HTTP/1.0 answer as read so far: the status and the body once
/// complete (by its `Content-Length`, else at the end), `None` while more
/// may come. `ended`: the server closed.
fn answer(raw: &[u8], ended: bool) -> Result<Option<(u16, String)>, String> {
    if raw.len() > MAX_ANSWER {
        return Err(format!("an answer of more than {MAX_ANSWER} bytes"));
    }
    let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if ended {
            Err("the answer ended before its headers".to_owned())
        } else {
            Ok(None)
        };
    };
    let head = raw.get(..at).unwrap_or_default();
    let body = raw.get(at + 4..).unwrap_or_default();
    let head =
        std::str::from_utf8(head).map_err(|_| "the answer's headers are not UTF-8".to_owned())?;
    let mut lines = head.split("\r\n");
    let status = status(lines.next().unwrap_or_default())?;
    let mut length = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("the header line {line:?}"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let n = value
                .parse::<usize>()
                .map_err(|_| format!("the content-length {value:?}"))?;
            length = Some(n);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(format!(
                "the transfer-encoding {value:?} (an HTTP/1.0 answer has none)"
            ));
        }
    }
    let body = match length {
        Some(n) => match body.get(..n) {
            Some(b) => b,
            None if ended => {
                return Err(format!(
                    "the answer ended after {} of {n} bytes",
                    body.len()
                ));
            }
            None => return Ok(None),
        },
        None if ended => body,
        None => return Ok(None),
    };
    let body = String::from_utf8(body.to_vec())
        .map_err(|_| "the answer's body is not UTF-8".to_owned())?;
    Ok(Some((status, body)))
}

/// `HTTP/1.x NNN reason`: the status.
fn status(line: &str) -> Result<u16, String> {
    let bad = || format!("the status line {line:?}");
    let (version, rest) = line.split_once(' ').ok_or_else(bad)?;
    let code = rest.split(' ').next().unwrap_or_default();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || code.len() != 3
        || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    code.parse().map_err(|_| bad())
}

#[cfg(test)]
mod tests;
