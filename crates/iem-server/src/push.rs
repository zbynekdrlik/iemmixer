//! Web Push encryption (RFC 8291) and VAPID delivery (reaperiem#133)

use crate::push_store::PushSubscription;
use aes_gcm::{Aes128Gcm, KeyInit, Nonce, aead::Aead};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use sha2::Sha256;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encrypt a push message payload per RFC 8291 (aes128gcm content encoding).
pub fn encrypt_payload(
    plaintext: &[u8],
    subscriber_p256dh_b64: &str,
    subscriber_auth_b64: &str,
) -> anyhow::Result<Vec<u8>> {
    let subscriber_pub_bytes = B64.decode(subscriber_p256dh_b64)?;
    let auth_secret = B64.decode(subscriber_auth_b64)?;

    let subscriber_pk = p256::PublicKey::from_sec1_bytes(&subscriber_pub_bytes)?;

    // Generate ephemeral ECDH key pair
    let ephemeral = p256::ecdh::EphemeralSecret::random(&mut rand_core::OsRng);
    let ephemeral_pk = p256::PublicKey::from(&ephemeral);
    let ephemeral_pub_bytes = ephemeral_pk.to_encoded_point(false);

    // ECDH shared secret
    let shared = ephemeral.diffie_hellman(&subscriber_pk);

    // Random salt
    let mut salt = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut salt);

    // Derive IKM: HKDF(salt=auth_secret, ikm=shared, info="WebPush: info\0" || ua_pub || as_pub)
    let mut info_ikm = Vec::with_capacity(131);
    info_ikm.extend_from_slice(b"WebPush: info\0");
    info_ikm.extend_from_slice(&subscriber_pub_bytes);
    info_ikm.extend_from_slice(ephemeral_pub_bytes.as_bytes());

    let hkdf_auth = Hkdf::<Sha256>::new(Some(&auth_secret), shared.raw_secret_bytes().as_slice());
    let mut ikm = [0u8; 32];
    hkdf_auth
        .expand(&info_ikm, &mut ikm)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Derive CEK and nonce from IKM + salt
    let hkdf_cek = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut cek = [0u8; 16];
    hkdf_cek
        .expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut nonce_bytes = [0u8; 12];
    hkdf_cek
        .expand(b"Content-Encoding: nonce\0", &mut nonce_bytes)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Pad plaintext (0x02 = final record delimiter)
    let mut padded = plaintext.to_vec();
    padded.push(0x02);

    // Encrypt with AES-128-GCM
    let cipher = Aes128Gcm::new_from_slice(&cek)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, padded.as_slice())
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt: {}", e))?;

    // Build aes128gcm body: salt(16) + rs(4) + idlen(1) + keyid(65) + ciphertext
    let rs: u32 = 4096;
    let mut body = Vec::with_capacity(86 + ciphertext.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&rs.to_be_bytes());
    body.push(65); // uncompressed P-256 point length
    body.extend_from_slice(ephemeral_pub_bytes.as_bytes());
    body.extend_from_slice(&ciphertext);

    Ok(body)
}

/// Build VAPID Authorization header components: (jwt_token, base64url_public_key).
pub fn build_vapid_header(
    vapid_private_key_b64: &str,
    endpoint: &str,
    subject: &str,
) -> anyhow::Result<(String, String)> {
    let raw = B64.decode(vapid_private_key_b64)?;
    let sk = p256::SecretKey::from_slice(&raw)?;

    // Audience = origin of the push endpoint
    let parsed = url::Url::parse(endpoint)?;
    let audience = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));

    // Build JWT claims
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let claims = serde_json::json!({
        "aud": audience,
        "exp": now + 12 * 3600,
        "sub": subject,
    });

    // Sign with ES256 using jsonwebtoken
    use p256::pkcs8::EncodePrivateKey;
    let pkcs8_der = sk.to_pkcs8_der()?;
    let encoding_key = jsonwebtoken::EncodingKey::from_ec_der(pkcs8_der.as_bytes());
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.typ = Some("JWT".into());
    let jwt = jsonwebtoken::encode(&header, &claims, &encoding_key)?;

    // Public key for k= parameter
    let pk = sk.public_key();
    let pub_b64 = B64.encode(pk.to_encoded_point(false).as_bytes());

    Ok((jwt, pub_b64))
}

/// Send a Web Push notification to a single subscription.
/// Returns Ok(true) if sent, Ok(false) if subscription expired (should be removed).
pub async fn send_push(
    client: &reqwest::Client,
    vapid_private_key_b64: &str,
    subject: &str,
    sub: &PushSubscription,
    payload: &[u8],
) -> anyhow::Result<bool> {
    let body = encrypt_payload(payload, &sub.p256dh, &sub.auth)?;
    let (jwt, pub_key) = build_vapid_header(vapid_private_key_b64, &sub.endpoint, subject)?;

    let resp = client
        .post(&sub.endpoint)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "aes128gcm")
        .header("TTL", "86400")
        .header("Authorization", format!("vapid t={}, k={}", jwt, pub_key))
        .body(body)
        .send()
        .await?;

    let status = resp.status().as_u16();
    match status {
        200..=202 => Ok(true),
        404 | 410 => Ok(false),
        _ => {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("push failed ({}): {}", status, text);
        }
    }
}

/// Send push to all engineer subscriptions. Removes expired ones.
pub async fn send_push_to_engineers(
    client: &reqwest::Client,
    vapid_key: &str,
    subject: &str,
    push_store: &std::sync::Arc<tokio::sync::RwLock<crate::push_store::PushStore>>,
    payload: &[u8],
) {
    let subs = {
        let store = push_store.read().await;
        store.all().to_vec()
    };

    if subs.is_empty() {
        return;
    }

    let mut expired = Vec::new();
    for sub in &subs {
        match send_push(client, vapid_key, subject, sub, payload).await {
            Ok(true) => {
                tracing::debug!(
                    "push sent to {}",
                    &sub.endpoint[..50.min(sub.endpoint.len())]
                );
            }
            Ok(false) => {
                tracing::info!("push subscription expired, removing");
                expired.push(sub.endpoint.clone());
            }
            Err(e) => {
                tracing::warn!("push send error: {}", e);
            }
        }
    }

    if !expired.is_empty() {
        let mut store = push_store.write().await;
        for endpoint in expired {
            store.remove_endpoint(&endpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_payload_produces_valid_output() {
        // Generate a fake subscriber key pair (simulating browser)
        let subscriber_sk = p256::SecretKey::random(&mut rand_core::OsRng);
        let subscriber_pk = subscriber_sk.public_key();
        let subscriber_pub_bytes = subscriber_pk.to_encoded_point(false);

        let mut auth_secret = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut auth_secret);

        let p256dh = B64.encode(subscriber_pub_bytes.as_bytes());
        let auth = B64.encode(auth_secret);

        let body = encrypt_payload(b"test payload", &p256dh, &auth).unwrap();

        // aes128gcm header: 16 (salt) + 4 (rs) + 1 (idlen) + 65 (keyid) = 86 bytes
        assert!(body.len() > 86);
        let rs = u32::from_be_bytes([body[16], body[17], body[18], body[19]]);
        assert_eq!(rs, 4096);
        assert_eq!(body[20], 65);
    }

    #[test]
    fn test_vapid_jwt_structure() {
        let sk = p256::SecretKey::random(&mut rand_core::OsRng);
        let key_b64 = B64.encode(sk.to_bytes());

        let (jwt, pub_key) = build_vapid_header(
            &key_b64,
            "https://fcm.googleapis.com/fcm/send/test",
            "mailto:admin@example.org",
        )
        .unwrap();

        assert_eq!(jwt.split('.').count(), 3);
        assert!(!pub_key.is_empty());
    }

    #[test]
    fn test_vapid_jwt_carries_the_configured_subject() {
        let sk = p256::SecretKey::random(&mut rand_core::OsRng);
        let key_b64 = B64.encode(sk.to_bytes());
        let (jwt, _) = build_vapid_header(
            &key_b64,
            "https://push.example.org/send/1",
            "mailto:ops@example.org",
        )
        .unwrap();
        let payload = jwt.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(payload).unwrap()).unwrap();
        assert_eq!(claims["sub"], "mailto:ops@example.org");
        assert_eq!(claims["aud"], "https://push.example.org");
    }

    /// Requests seen by the fake push service: (path, Authorization, Content-Encoding).
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

    /// Answers `POST /<status>` with that HTTP status and records the request.
    async fn answer_with_status(
        axum::extract::State(seen): axum::extract::State<Seen>,
        axum::extract::Path(status): axum::extract::Path<u16>,
        headers: axum::http::HeaderMap,
    ) -> axum::http::StatusCode {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        seen.lock().unwrap().push((
            format!("/{status}"),
            header("authorization"),
            header("content-encoding"),
        ));
        axum::http::StatusCode::from_u16(status).unwrap()
    }

    /// A stand-in for the browser vendor's push service (an external network
    /// service): `POST /<status>` answers with that HTTP status.
    async fn fake_push_service() -> (String, Seen) {
        let seen = Seen::default();
        let app = axum::Router::new()
            .route("/{status}", axum::routing::post(answer_with_status))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn subscription(endpoint: String) -> PushSubscription {
        let browser_key = p256::SecretKey::random(&mut rand_core::OsRng);
        PushSubscription {
            endpoint,
            p256dh: B64.encode(browser_key.public_key().to_encoded_point(false).as_bytes()),
            auth: B64.encode([7u8; 16]),
        }
    }

    fn vapid_private_key() -> String {
        B64.encode(p256::SecretKey::random(&mut rand_core::OsRng).to_bytes())
    }

    const SUBJECT: &str = "mailto:ops@example.org";

    #[tokio::test]
    async fn send_push_tells_delivered_from_expired() {
        let (base, seen) = fake_push_service().await;
        let client = reqwest::Client::new();
        let key = vapid_private_key();
        let send = |status: u16| {
            let sub = subscription(format!("{base}/{status}"));
            let (client, key) = (client.clone(), key.clone());
            async move { send_push(&client, &key, SUBJECT, &sub, b"alert").await }
        };
        assert!(send(201).await.unwrap(), "2xx: delivered");
        assert!(!send(410).await.unwrap(), "410: subscription gone");
        assert!(!send(404).await.unwrap(), "404: subscription gone");
        assert!(send(500).await.is_err(), "other statuses are errors");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        let (path, authorization, content_encoding) = &seen[0];
        assert_eq!(path, "/201");
        assert!(authorization.starts_with("vapid t="), "{authorization}");
        assert_eq!(content_encoding, "aes128gcm");
    }

    #[tokio::test]
    async fn expired_engineer_subscriptions_are_removed() {
        let (base, seen) = fake_push_service().await;
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::push_store::PushStore::load(dir.path()),
        ));
        let live = subscription(format!("{base}/201"));
        let gone = subscription(format!("{base}/410"));
        {
            let mut s = store.write().await;
            s.add(live.clone()).unwrap();
            s.add(gone).unwrap();
        }
        send_push_to_engineers(
            &reqwest::Client::new(),
            &vapid_private_key(),
            SUBJECT,
            &store,
            b"alert",
        )
        .await;
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "both subscriptions were tried"
        );
        assert_eq!(store.read().await.all(), std::slice::from_ref(&live));
    }
}
