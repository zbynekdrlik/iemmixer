//! Web Push encryption (RFC 8291) and VAPID delivery (reaperiem#133)

use crate::push_store::PushSubscription;
use aes_gcm::aead::{Aead, Nonce};
use aes_gcm::{Aes128Gcm, Key, KeyInit};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use sha2::Sha256;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encrypt a push message payload per RFC 8291 (aes128gcm content encoding),
/// with a new ECDH key and a new salt for every message.
pub fn encrypt_payload(
    plaintext: &[u8],
    subscriber_p256dh_b64: &str,
    subscriber_auth_b64: &str,
) -> anyhow::Result<Vec<u8>> {
    // The message's own ECDH key (from the OS's RNG), dropped (zeroed) after it
    let ephemeral = p256::SecretKey::try_generate()?;

    // Random salt
    let mut salt = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut salt);

    encrypt_with(
        plaintext,
        subscriber_p256dh_b64,
        subscriber_auth_b64,
        &ephemeral,
        salt,
    )
}

/// RFC 8291 with the sender's ECDH key and the salt given: [`encrypt_payload`]
/// draws both, the tests give the RFC's example values.
fn encrypt_with(
    plaintext: &[u8],
    subscriber_p256dh_b64: &str,
    subscriber_auth_b64: &str,
    ephemeral: &p256::SecretKey,
    salt: [u8; 16],
) -> anyhow::Result<Vec<u8>> {
    let subscriber_pub_bytes = B64.decode(subscriber_p256dh_b64)?;
    let auth_secret = B64.decode(subscriber_auth_b64)?;

    let subscriber_pk = p256::PublicKey::from_sec1_bytes(&subscriber_pub_bytes)?;
    let ephemeral_pub_bytes = ephemeral.public_key().to_sec1_point(false);

    // ECDH shared secret
    let shared = ephemeral.diffie_hellman(&subscriber_pk);

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
    let cipher = Aes128Gcm::new(&Key::<Aes128Gcm>::from(cek));
    let nonce = Nonce::<Aes128Gcm>::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, padded.as_slice())
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

    // Sign with ES256 (`{"typ":"JWT","alg":"ES256"}`, r || s)
    let jwt = iem_core::jwt::encode_es256(&claims, &sk)?;

    // Public key for k= parameter
    let pk = sk.public_key();
    let pub_b64 = B64.encode(pk.to_sec1_point(false).as_bytes());

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

/// Send push to all engineer subscriptions. Removes expired ones. Returns
/// how many took it.
pub async fn send_push_to_engineers(
    client: &reqwest::Client,
    vapid_key: &str,
    subject: &str,
    push_store: &std::sync::Arc<tokio::sync::RwLock<crate::push_store::PushStore>>,
    payload: &[u8],
) -> usize {
    let subs = {
        let store = push_store.read().await;
        store.all().to_vec()
    };

    let mut sent = 0;
    let mut expired = Vec::new();
    for sub in &subs {
        match send_push(client, vapid_key, subject, sub, payload).await {
            Ok(true) => {
                sent += 1;
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
    sent
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn test_encrypt_payload_produces_valid_output() {
        // Generate a fake subscriber key pair (simulating browser)
        let subscriber_sk = p256::SecretKey::generate();
        let subscriber_pk = subscriber_sk.public_key();
        let subscriber_pub_bytes = subscriber_pk.to_sec1_point(false);

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

        // Every message has its own salt and ECDH key.
        let again = encrypt_payload(b"test payload", &p256dh, &auth).unwrap();
        assert_ne!(body[..16], again[..16], "salt");
        assert_ne!(body[21..86], again[21..86], "the sender's key");
    }

    /// RFC 8291 §5 and Appendix A: the example message, the sender's ECDH
    /// scalar, the receiver's point, the auth value and the salt, and the
    /// body they encrypt to (the RFC's line breaks removed).
    const RFC8291_PLAINTEXT: &[u8] = b"When I grow up, I want to be a watermelon";
    const RFC8291_SENDER_SCALAR: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const RFC8291_RECEIVER_POINT: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const RFC8291_AUTH_VALUE: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const RFC8291_SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const RFC8291_BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    /// The ECDH, both HKDF steps, AES-128-GCM and the body's layout against
    /// the RFC's published example (#74 moved every crate under them).
    #[test]
    fn the_rfc_8291_example_encrypts_to_its_published_body() {
        let scalar = B64.decode(RFC8291_SENDER_SCALAR).unwrap();
        let sender = p256::SecretKey::from_slice(&scalar).unwrap();
        let salt: [u8; 16] = B64.decode(RFC8291_SALT).unwrap().try_into().unwrap();
        let body = encrypt_with(
            RFC8291_PLAINTEXT,
            RFC8291_RECEIVER_POINT,
            RFC8291_AUTH_VALUE,
            &sender,
            salt,
        )
        .unwrap();
        assert_eq!(B64.encode(body), RFC8291_BODY);
    }

    /// What the push service checks (RFC 8292): the VAPID JWT is ES256 over
    /// its first two parts with the key the `k=` parameter names, expires
    /// in 12 h, and its header is the one jsonwebtoken wrote.
    #[test]
    fn the_vapid_jwt_verifies_with_the_key_it_names() {
        use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
        let sk = p256::SecretKey::generate();
        let (jwt, k) = build_vapid_header(
            &B64.encode(sk.to_bytes()),
            "https://push.example.org/send/1",
            SUBJECT,
        )
        .unwrap();
        let header = iem_core::jwt::header_of(&jwt).unwrap();
        assert_eq!(
            serde_json::Value::Object(header),
            serde_json::json!({"typ": "JWT", "alg": "ES256"})
        );
        let (input, signature) = jwt.rsplit_once('.').unwrap();
        let verifying = VerifyingKey::from_sec1_bytes(&B64.decode(k.as_bytes()).unwrap()).unwrap();
        let signature = Signature::from_slice(&B64.decode(signature).unwrap()).unwrap();
        assert!(verifying.verify(input.as_bytes(), &signature).is_ok());
        let other =
            VerifyingKey::from(&p256::ecdsa::SigningKey::from(&p256::SecretKey::generate()));
        assert!(other.verify(input.as_bytes(), &signature).is_err());
        let claims = input.split_once('.').unwrap().1;
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(claims).unwrap()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let exp = claims["exp"].as_u64().unwrap();
        assert!(exp.abs_diff(now + 12 * 3600) < 5, "exp {exp}, now {now}");
    }

    #[test]
    fn test_vapid_jwt_structure() {
        let sk = p256::SecretKey::generate();
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
        let sk = p256::SecretKey::generate();
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
    pub(crate) type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

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
    pub(crate) async fn fake_push_service() -> (String, Seen) {
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

    pub(crate) fn subscription(endpoint: String) -> PushSubscription {
        let browser_key = p256::SecretKey::generate();
        PushSubscription {
            endpoint,
            p256dh: B64.encode(browser_key.public_key().to_sec1_point(false).as_bytes()),
            auth: B64.encode([7u8; 16]),
        }
    }

    pub(crate) fn vapid_private_key() -> String {
        B64.encode(p256::SecretKey::generate().to_bytes())
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
        let sent = send_push_to_engineers(
            &reqwest::Client::new(),
            &vapid_private_key(),
            SUBJECT,
            &store,
            b"alert",
        )
        .await;
        assert_eq!(sent, 1, "only the live one took it");
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "both subscriptions were tried"
        );
        assert_eq!(store.read().await.all(), std::slice::from_ref(&live));
    }
}
