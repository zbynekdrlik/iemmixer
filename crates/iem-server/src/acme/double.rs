// Ported from fohmixer 9f3d440 crates/fohmixer-hub/src/acme/double.rs (iemmixer#91).
//! A local double of an ACME CA (RFC 8555) for the ACME tests: directory,
//! nonces, accounts, orders, one DNS-01 authorization per order, finalize
//! and the certificate download. It checks what a CA checks that the
//! server's code decides: every request's JWS signature (ES256, the
//! account's key), its nonce and URL; and the DNS-01 record — the TXT record
//! at `_acme-challenge.<name>` in the Cloudflare double must hold the key
//! authorization's digest when the challenge is answered. The test CA
//! signs the CSR. A test can refuse one name's challenges or new orders (a
//! rate limit) and leave every other name's alone.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use crate::cloudflare::double::Shared as Dns;
use crate::tls_cert::test_certs::TestCa;

/// The double's state.
pub struct Ca {
    pub base: String,
    pub dns: Dns,
    pub test_ca: TestCa,
    /// When set, the check of every challenge fails (the record is ignored).
    pub refuse_challenges: bool,
    /// Names whose challenge check fails (the record is ignored): a DNS-01
    /// error of that name only.
    pub refused: BTreeSet<String>,
    /// Names whose new orders are refused as Let's Encrypt refuses one over
    /// its rate limit (429 `rateLimited`).
    pub rate_limited: BTreeSet<String>,
    /// Account URL → its public key (x, y) and thumbprint.
    pub accounts: BTreeMap<String, (Vec<u8>, String)>,
    /// Every request: `POST /new-order` etc.
    pub requests: Vec<String>,
    /// Every new-account payload.
    pub new_accounts: Vec<Value>,
    nonces: BTreeSet<String>,
    next: u64,
    /// Order id → (name, status, authorization id, certificate PEM).
    orders: BTreeMap<u64, (String, &'static str, u64, Option<String>)>,
    /// Authorization id → (name, token, status, account URL).
    authzs: BTreeMap<u64, (String, String, &'static str, String)>,
}

pub type SharedCa = Arc<Mutex<Ca>>;

impl Ca {
    fn nonce(&mut self) -> String {
        self.next += 1;
        let nonce = format!("nonce-{}", self.next);
        self.nonces.insert(nonce.clone());
        nonce
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn order_json(&self, id: u64) -> Value {
        let (name, status, authz, cert) = &self.orders[&id];
        let mut order = json!({
            "status": status,
            "identifiers": [{"type": "dns", "value": name}],
            "authorizations": [format!("{}/authz/{authz}", self.base)],
            "finalize": format!("{}/finalize/{id}", self.base),
        });
        if cert.is_some() {
            order["certificate"] = json!(format!("{}/cert/{id}", self.base));
        }
        order
    }

    fn challenge_json(&self, id: u64) -> Value {
        let (_, token, status, _) = &self.authzs[&id];
        let mut challenge = json!({
            "type": "dns-01",
            "url": format!("{}/chall/{id}", self.base),
            "token": token,
            "status": status,
        });
        if *status == "invalid" {
            challenge["error"] = json!({
                "type": "urn:ietf:params:acme:error:unauthorized",
                "detail": "no TXT record with the key authorization",
            });
        }
        challenge
    }

    /// The order's status from its authorization's (before finalize).
    fn order_status(&self, id: u64) -> &'static str {
        let (_, status, authz, _) = &self.orders[&id];
        match (*status, self.authzs[authz].2) {
            ("valid", _) => "valid",
            (_, "valid") => "ready",
            (_, "invalid") => "invalid",
            _ => "pending",
        }
    }
}

/// A problem document.
fn problem(status: StatusCode, kind: &str, detail: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        json!({"type": format!("urn:ietf:params:acme:error:{kind}"), "detail": detail}).to_string(),
    )
        .into_response()
}

fn b64(text: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|e| format!("base64: {e}"))
}

/// The RFC 7638 thumbprint of an EC P-256 JWK.
fn thumbprint(x: &str, y: &str) -> String {
    let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
    URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        canonical.as_bytes(),
    ))
}

/// A checked JWS request: the account URL (`kid`, or the new account's
/// own), and the payload (`Value::Null` for a POST-as-GET).
/// A checked request: its account URL, its payload, and a new account's key
/// and thumbprint.
type Checked = (String, Value, Option<(Vec<u8>, String)>);

fn check_jws(ca: &mut Ca, url: &str, body: &[u8]) -> Result<Checked, String> {
    let jws: Value = serde_json::from_slice(body).map_err(|e| format!("not a JWS: {e}"))?;
    let (protected64, payload64, signature64) = (
        jws["protected"].as_str().unwrap_or(""),
        jws["payload"].as_str().unwrap_or(""),
        jws["signature"].as_str().unwrap_or(""),
    );
    let protected: Value =
        serde_json::from_slice(&b64(protected64)?).map_err(|e| format!("protected: {e}"))?;
    if protected["alg"] != "ES256" {
        return Err(format!("alg {}", protected["alg"]));
    }
    if protected["url"] != url {
        return Err(format!("url {} for {url}", protected["url"]));
    }
    let nonce = protected["nonce"].as_str().unwrap_or("");
    if !ca.nonces.remove(nonce) {
        return Err(format!("unknown or used nonce {nonce}"));
    }
    let (account, key, new) = if let Some(kid) = protected["kid"].as_str() {
        let (key, _) = ca.accounts.get(kid).ok_or(format!("no account {kid}"))?;
        (kid.to_string(), key.clone(), None)
    } else {
        let jwk = &protected["jwk"];
        let (x, y) = (
            jwk["x"].as_str().unwrap_or(""),
            jwk["y"].as_str().unwrap_or(""),
        );
        let mut key = vec![4u8];
        key.extend(b64(x)?);
        key.extend(b64(y)?);
        let n = ca.id();
        let url = format!("{}/acct/{n}", ca.base);
        (url, key.clone(), Some((key, thumbprint(x, y))))
    };
    let signed = format!("{protected64}.{payload64}");
    ring::signature::UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        key.as_slice(),
    )
    .verify(signed.as_bytes(), &b64(signature64)?)
    .map_err(|_| "bad signature".to_string())?;
    let payload = if payload64.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&b64(payload64)?).map_err(|e| format!("payload: {e}"))?
    };
    Ok((account, payload, new))
}

/// A JSON answer with a fresh nonce (and a `Location`).
fn reply(ca: &mut Ca, status: StatusCode, location: Option<String>, body: Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert("replay-nonce", HeaderValue::from_str(&ca.nonce()).unwrap());
    if let Some(location) = location {
        headers.insert(header::LOCATION, HeaderValue::from_str(&location).unwrap());
    }
    response
}

async fn directory(State(ca): State<SharedCa>) -> axum::Json<Value> {
    let ca = ca.lock().unwrap();
    axum::Json(json!({
        "newNonce": format!("{}/new-nonce", ca.base),
        "newAccount": format!("{}/new-account", ca.base),
        "newOrder": format!("{}/new-order", ca.base),
        "revokeCert": format!("{}/revoke", ca.base),
        "keyChange": format!("{}/key-change", ca.base),
    }))
}

async fn new_nonce(State(ca): State<SharedCa>) -> Response {
    let mut ca = ca.lock().unwrap();
    let nonce = ca.nonce();
    (
        [
            ("replay-nonce", nonce),
            ("cache-control", "no-store".into()),
        ],
        "",
    )
        .into_response()
}

/// Every JWS POST: `/{kind}` or `/{kind}/{id}`.
async fn acme_post(
    State(ca): State<SharedCa>,
    Path(path): Path<String>,
    _h: HeaderMap,
    body: Bytes,
) -> Response {
    let mut guard = ca.lock().unwrap();
    let ca = &mut *guard;
    ca.requests.push(format!("POST /{path}"));
    let url = format!("{}/{path}", ca.base);
    let (account, payload, new) = match check_jws(ca, &url, &body) {
        Ok(checked) => checked,
        Err(why) => return problem(StatusCode::BAD_REQUEST, "malformed", &why),
    };
    let (kind, id) = match path.split_once('/') {
        Some((kind, id)) => (kind.to_string(), id.parse::<u64>().unwrap_or(0)),
        None => (path.clone(), 0),
    };
    match kind.as_str() {
        "new-account" => {
            let Some(key) = new else {
                return problem(
                    StatusCode::BAD_REQUEST,
                    "malformed",
                    "a new account needs a jwk",
                );
            };
            assert_eq!(payload["termsOfServiceAgreed"], json!(true));
            ca.new_accounts.push(payload);
            ca.accounts.insert(account.clone(), key);
            reply(
                ca,
                StatusCode::CREATED,
                Some(account),
                json!({"status": "valid"}),
            )
        }
        "new-order" => {
            let name = payload["identifiers"][0]["value"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if ca.rate_limited.contains(&name) {
                return problem(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rateLimited",
                    &format!("too many certificates already issued for {name}"),
                );
            }
            // A valid authorization of this account for the name is reused,
            // as Let's Encrypt does for 30 days: no new challenge.
            let valid = ca
                .authzs
                .iter()
                .find(|(_, (n, _, status, owner))| {
                    *n == name && *status == "valid" && *owner == account
                })
                .map(|(id, _)| *id);
            let authz = match valid {
                Some(id) => id,
                None => {
                    let id = ca.id();
                    let token = format!("token{id}");
                    ca.authzs
                        .insert(id, (name.clone(), token, "pending", account));
                    id
                }
            };
            let order = ca.id();
            ca.orders.insert(order, (name, "pending", authz, None));
            let location = format!("{}/order/{order}", ca.base);
            let body = ca.order_json(order);
            reply(ca, StatusCode::CREATED, Some(location), body)
        }
        "authz" => {
            let (name, _, status, _) = ca.authzs[&id].clone();
            let body = json!({
                "identifier": {"type": "dns", "value": name},
                "status": status,
                "challenges": [ca.challenge_json(id)],
            });
            reply(ca, StatusCode::OK, None, body)
        }
        "chall" => {
            // The CA's check: the TXT record must hold the digest of
            // "<token>.<the account key's thumbprint>".
            let (name, token, _, owner) = ca.authzs[&id].clone();
            let thumb = ca.accounts[&owner].1.clone();
            let expected = URL_SAFE_NO_PAD.encode(ring::digest::digest(
                &ring::digest::SHA256,
                format!("{token}.{thumb}").as_bytes(),
            ));
            let fqdn = format!("_acme-challenge.{name}");
            let found = ca
                .dns
                .lock()
                .unwrap()
                .records
                .values()
                .any(|(n, content, _)| *n == fqdn && *content == expected);
            let status = if found && !ca.refuse_challenges && !ca.refused.contains(&name) {
                "valid"
            } else {
                "invalid"
            };
            ca.authzs.get_mut(&id).unwrap().2 = status;
            let body = ca.challenge_json(id);
            reply(ca, StatusCode::OK, None, body)
        }
        "order" => {
            let status = ca.order_status(id);
            ca.orders.get_mut(&id).unwrap().1 = status;
            let body = ca.order_json(id);
            reply(ca, StatusCode::OK, None, body)
        }
        "finalize" => {
            if ca.order_status(id) != "ready" {
                return problem(StatusCode::FORBIDDEN, "orderNotReady", "not ready");
            }
            let name = ca.orders[&id].0.clone();
            let csr = match payload["csr"].as_str().map(b64) {
                Some(Ok(der)) => der,
                _ => return problem(StatusCode::BAD_REQUEST, "badCSR", "no csr"),
            };
            let chain = match ca.test_ca.sign_csr(&csr, &name) {
                Ok(cert) => format!("{cert}{}", ca.test_ca.pem),
                Err(why) => return problem(StatusCode::BAD_REQUEST, "badCSR", &why),
            };
            let order = ca.orders.get_mut(&id).unwrap();
            order.1 = "valid";
            order.3 = Some(chain);
            let body = ca.order_json(id);
            reply(ca, StatusCode::OK, None, body)
        }
        "cert" => {
            let chain = ca.orders[&id].3.clone().unwrap_or_default();
            let mut response = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/pem-certificate-chain")],
                chain,
            )
                .into_response();
            let nonce = ca.nonce();
            response
                .headers_mut()
                .insert("replay-nonce", HeaderValue::from_str(&nonce).unwrap());
            response
        }
        other => problem(StatusCode::NOT_FOUND, "malformed", &format!("no {other}")),
    }
}

/// Starts the double next to the Cloudflare double `dns`: its directory URL
/// and its state.
pub async fn start(dns: Dns) -> (String, SharedCa) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let ca: SharedCa = Arc::new(Mutex::new(Ca {
        base: base.clone(),
        dns,
        test_ca: TestCa::new(),
        refuse_challenges: false,
        refused: BTreeSet::new(),
        rate_limited: BTreeSet::new(),
        accounts: BTreeMap::new(),
        requests: Vec::new(),
        new_accounts: Vec::new(),
        nonces: BTreeSet::new(),
        next: 0,
        orders: BTreeMap::new(),
        authzs: BTreeMap::new(),
    }));
    let app = Router::new()
        .route("/directory", get(directory))
        .route("/new-nonce", get(new_nonce).head(new_nonce))
        .route("/{*path}", post(acme_post))
        .with_state(Arc::clone(&ca));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/directory"), ca)
}
