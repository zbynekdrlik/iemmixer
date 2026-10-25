//! JSON Web Tokens (RFC 7519) as iemmixer signs and reads them (#74): the
//! login tokens, HS256 with the server's `jwt_secret` (the server's login,
//! the soak client's own tokens), and with the `vapid` feature the Web Push
//! VAPID token (RFC 8292), ES256 with the VAPID key.
//!
//! In place of jsonwebtoken 11.1 (`rust_crypto`), whose p256/p384 0.13 held
//! crypto-bigint 0.5 (RUSTSEC-2026-0387). A token is the one jsonwebtoken
//! wrote, byte for byte, so the tokens browsers hold stay valid: the header
//! [`HS256_HEADER`] ([`ES256_HEADER`] for VAPID), the claims as serde_json
//! writes them, each base64url without padding, then the signature over
//! `<header>.<claims>`: HS256 is HMAC-SHA-256 keyed with the secret's bytes;
//! ES256 is the deterministic (RFC 6979) P-256 signature as `r || s`, 64
//! bytes (RFC 7518 §3.4).
//!
//! [`decode_hs256`] reads a token as jsonwebtoken's `decode` with
//! `Validation::default()` did: the header's `alg` must be `HS256` (pinned:
//! `none` and every other algorithm are refused before the signature is
//! looked at), the HMAC is compared in constant time (`hmac`'s
//! `verify_slice`), `exp` must be a whole number of seconds no more than
//! [`LEEWAY_SECS`] in the past, and an `aud` claim is refused (no token of
//! ours names an audience); `nbf` and `iat` are not checked. Stricter in
//! two points, neither of which a token of ours meets: a header with `crit`
//! is refused (RFC 7515 §4.1.11), and the header and the claims must be
//! JSON objects. No error holds a part of the token.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sha2::Sha256;

/// How far in the past a token's `exp` may lie and the token still be
/// taken: jsonwebtoken's default leeway, which the login tokens were always
/// read with.
pub const LEEWAY_SECS: u64 = 60;

/// The header of an HS256 token, as jsonwebtoken's `Header::default()`
/// wrote it.
pub const HS256_HEADER: &str = r#"{"typ":"JWT","alg":"HS256"}"#;

/// The header of the VAPID token, as jsonwebtoken's `Header::new(ES256)`
/// wrote it.
pub const ES256_HEADER: &str = r#"{"typ":"JWT","alg":"ES256"}"#;

type HmacSha256 = Hmac<Sha256>;

/// Why a token was not made or not taken. Never holds a part of the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Not three base64url parts, or a header or claims part that is no
    /// JSON object, or a header without an `alg` string.
    #[error("the token is not three base64url parts holding JSON objects")]
    Malformed,
    /// The header names another algorithm than the one expected, or carries
    /// `crit`.
    #[error("the token's algorithm is not the expected one")]
    Algorithm,
    /// The signature does not verify with the key.
    #[error("the token's signature does not verify")]
    Signature,
    /// `exp` lies more than [`LEEWAY_SECS`] in the past.
    #[error("the token has expired")]
    Expired,
    /// The claims have no whole-second `exp`, carry an `aud`, or do not fit
    /// the type asked for.
    #[error("the token's claims are not the expected ones")]
    Claims,
    /// The claims could not be written, or the key could not sign.
    #[error("the token could not be made")]
    Encode,
}

/// An HS256 token of `claims`, signed with `secret`'s bytes.
pub fn encode_hs256<T: Serialize + ?Sized>(claims: &T, secret: &[u8]) -> Result<String, JwtError> {
    let input = signing_input(HS256_HEADER, claims)?;
    let tag = mac(secret, &input).ok_or(JwtError::Encode)?.finalize();
    Ok(signed(input, &tag.into_bytes()))
}

/// An ES256 token of `claims`, signed with `key` (the VAPID JWT).
#[cfg(feature = "vapid")]
pub fn encode_es256<T: Serialize + ?Sized>(
    claims: &T,
    key: &p256::SecretKey,
) -> Result<String, JwtError> {
    use p256::ecdsa::{Signature, SigningKey, signature::Signer};
    let input = signing_input(ES256_HEADER, claims)?;
    let signature: Signature = SigningKey::from(key)
        .try_sign(input.as_bytes())
        .map_err(|_| JwtError::Encode)?;
    Ok(signed(input, &signature.to_bytes()))
}

/// The claims of the HS256 `token` signed with `secret`'s bytes, read at
/// the Unix second `now` (the rules are the module's).
pub fn decode_hs256<T: DeserializeOwned>(
    token: &str,
    secret: &[u8],
    now: u64,
) -> Result<T, JwtError> {
    let (input, signature) = token.rsplit_once('.').ok_or(JwtError::Malformed)?;
    let (header, claims) = input.split_once('.').ok_or(JwtError::Malformed)?;
    let header = object(header)?;
    let alg = header
        .get("alg")
        .and_then(Value::as_str)
        .ok_or(JwtError::Malformed)?;
    if alg != "HS256" || header.contains_key("crit") {
        return Err(JwtError::Algorithm);
    }
    mac(secret, input)
        .ok_or(JwtError::Signature)?
        .verify_slice(&unbase(signature)?)
        .map_err(|_| JwtError::Signature)?;
    let claims = object(claims)?;
    let exp = claims
        .get("exp")
        .and_then(Value::as_u64)
        .ok_or(JwtError::Claims)?;
    if claims.get("aud").is_some_and(|aud| !aud.is_null()) {
        return Err(JwtError::Claims);
    }
    if exp < now.saturating_sub(LEEWAY_SECS) {
        return Err(JwtError::Expired);
    }
    serde_json::from_value(Value::Object(claims)).map_err(|_| JwtError::Claims)
}

/// The header of `token`, unchecked (its signature is not looked at): for
/// tests and diagnostics, never for a decision.
pub fn header_of(token: &str) -> Result<Map<String, Value>, JwtError> {
    let (header, _) = token.split_once('.').ok_or(JwtError::Malformed)?;
    object(header)
}

/// `<header>.<claims>`, each base64url without padding.
fn signing_input<T: Serialize + ?Sized>(header: &str, claims: &T) -> Result<String, JwtError> {
    let claims = serde_json::to_vec(claims).map_err(|_| JwtError::Encode)?;
    Ok(format!("{}.{}", B64.encode(header), B64.encode(claims)))
}

/// `input` with its signature appended.
fn signed(input: String, signature: &[u8]) -> String {
    format!("{input}.{}", B64.encode(signature))
}

/// HMAC-SHA-256 keyed with `secret`, fed with `input` (HMAC takes a key of
/// any length, so `None` is not seen).
fn mac(secret: &[u8], input: &str) -> Option<HmacSha256> {
    let mut state = HmacSha256::new_from_slice(secret).ok()?;
    state.update(input.as_bytes());
    Some(state)
}

/// The JSON object a part holds.
fn object(part: &str) -> Result<Map<String, Value>, JwtError> {
    serde_json::from_slice(&unbase(part)?).map_err(|_| JwtError::Malformed)
}

/// A part's bytes; a part that is no unpadded base64url (a dot in it too)
/// is malformed.
fn unbase(part: &str) -> Result<Vec<u8>, JwtError> {
    B64.decode(part).map_err(|_| JwtError::Malformed)
}

#[cfg(test)]
mod tests;
