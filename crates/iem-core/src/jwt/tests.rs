//! The tokens jsonwebtoken wrote and read, pinned by known answers computed
//! outside this crate (Python's `hmac`, a pure-Python RFC 6979 that
//! reproduces RFC 6979 A.2.5, checked again with OpenSSL), and every rule
//! of the reading. Values are synthetic (P6). No part of a token is one
//! string literal here: the parts are joined at run time.

use super::*;
use crate::AuthClaims;

const SECRET: &[u8] = b"synthetic-jwt-secret";
/// The reading's Unix second.
const NOW: u64 = 1_999_999_500;

fn claims(exp: u64) -> AuthClaims {
    AuthClaims {
        sub: "member1".into(),
        engineer: false,
        exp,
        iat: 1_999_999_000,
    }
}

fn token(exp: u64) -> String {
    encode_hs256(&claims(exp), SECRET).unwrap()
}

fn read(token: &str, secret: &[u8]) -> Result<AuthClaims, JwtError> {
    decode_hs256(token, secret, NOW)
}

fn err(token: &str) -> JwtError {
    read(token, SECRET).unwrap_err()
}

/// A token of the raw `header` and `claims` JSON, HS256-signed with
/// `SECRET` whatever the header says.
fn forge(header: &str, claims: &str) -> String {
    let input = format!("{}.{}", B64.encode(header), B64.encode(claims));
    let tag = mac(SECRET, &input).unwrap().finalize().into_bytes();
    signed(input, &tag)
}

/// The three parts of `token`.
fn parts(token: &str) -> [String; 3] {
    let parts: Vec<String> = token.split('.').map(str::to_owned).collect();
    parts.try_into().unwrap()
}

const BODY: &str = r#"{"sub":"member1","engineer":true,"exp":2000000000,"iat":1}"#;

/// `encode_hs256(&claims(2_000_000_000), SECRET)`: HS256_HEADER and
/// `{"sub":"member1","engineer":false,"exp":2000000000,"iat":1999999000}`.
const KAT_HEADER: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9";
const KAT_CLAIMS: &str =
    "eyJzdWIiOiJtZW1iZXIxIiwiZW5naW5lZXIiOmZhbHNlLCJleHAiOjIwMDAwMDAwMDAsImlhdCI6MTk5OTk5OTAwMH0";
const KAT_SIGNATURE: &str = "w7vZsB7877hjwNisn3S5jleXnX8VzocSXo4jE8CN2rc";

#[test]
fn an_hs256_token_is_byte_for_byte_the_one_jsonwebtoken_wrote() {
    assert_eq!(
        token(2_000_000_000),
        format!("{KAT_HEADER}.{KAT_CLAIMS}.{KAT_SIGNATURE}")
    );
    assert_eq!(B64.decode(KAT_HEADER).unwrap(), HS256_HEADER.as_bytes());
    let header = header_of(&token(2_000_000_000)).unwrap();
    assert_eq!(header.len(), 2);
    assert_eq!(
        (header["typ"].as_str(), header["alg"].as_str()),
        (Some("JWT"), Some("HS256"))
    );
}

#[test]
fn a_token_jsonwebtoken_issued_reads_back() {
    let issued = format!("{KAT_HEADER}.{KAT_CLAIMS}.{KAT_SIGNATURE}");
    let got = read(&issued, SECRET).unwrap();
    assert_eq!(
        (got.sub.as_str(), got.engineer, got.exp, got.iat),
        ("member1", false, 2_000_000_000, 1_999_999_000)
    );
}

/// RFC 7515 Appendix A.1: the published HS256 example, its header written
/// with a CRLF and a blank, its 64-byte HMAC octets.
const RFC7515_A1: [&str; 3] = [
    "eyJ0eXAiOiJKV1QiLA0KICJhbGciOiJIUzI1NiJ9",
    "eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ",
    "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
];
const RFC7515_A1_OCTETS: &str =
    "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow";
const RFC7515_A1_EXP: u64 = 1_300_819_380;

#[test]
fn the_rfc_7515_hs256_example_verifies_and_expires() {
    let example = RFC7515_A1.join(".");
    let octets = B64.decode(RFC7515_A1_OCTETS).unwrap();
    let got: Value = decode_hs256(&example, &octets, RFC7515_A1_EXP).unwrap();
    assert_eq!(got["iss"], "joe");
    assert_eq!(got["http://example.com/is_root"], Value::Bool(true));
    let late = RFC7515_A1_EXP + LEEWAY_SECS + 1;
    assert_eq!(
        decode_hs256::<Value>(&example, &octets, late),
        Err(JwtError::Expired)
    );
    assert_eq!(
        decode_hs256::<Value>(&example, SECRET, RFC7515_A1_EXP),
        Err(JwtError::Signature)
    );
}

#[test]
fn a_token_reads_back_as_its_claims() {
    let got = read(&token(NOW + 3_600), SECRET).unwrap();
    assert_eq!(
        (got.sub.as_str(), got.engineer, got.exp, got.iat),
        ("member1", false, NOW + 3_600, 1_999_999_000)
    );
}

#[test]
fn another_secret_is_refused() {
    let token = token(NOW + 3_600);
    assert_eq!(
        read(&token, b"another-synthetic-secret").unwrap_err(),
        JwtError::Signature
    );
    assert_eq!(read(&token, b"").unwrap_err(), JwtError::Signature);
    assert_eq!(
        read(&token, &SECRET[..SECRET.len() - 1]).unwrap_err(),
        JwtError::Signature
    );
}

#[test]
fn only_hs256_is_taken_whatever_the_signature() {
    assert!(
        read(&forge(HS256_HEADER, BODY), SECRET).is_ok(),
        "the forge signs as HS256"
    );
    for header in [
        r#"{"typ":"JWT","alg":"none"}"#,
        r#"{"typ":"JWT","alg":"HS384"}"#,
        r#"{"typ":"JWT","alg":"HS512"}"#,
        r#"{"typ":"JWT","alg":"ES256"}"#,
        r#"{"typ":"JWT","alg":"RS256"}"#,
        r#"{"typ":"JWT","alg":"hs256"}"#,
        r#"{"typ":"JWT","alg":"HS256 "}"#,
    ] {
        assert_eq!(err(&forge(header, BODY)), JwtError::Algorithm, "{header}");
    }
    // An unsecured token (RFC 7519 §6): `none` and an empty signature.
    let unsecured = format!("{}.{}.", B64.encode(r#"{"alg":"none"}"#), B64.encode(BODY));
    assert_eq!(err(&unsecured), JwtError::Algorithm);
}

#[test]
fn a_header_without_an_alg_string_is_malformed() {
    for header in [
        r#"{"typ":"JWT"}"#,
        r#"{"typ":"JWT","alg":null}"#,
        r#"{"typ":"JWT","alg":256}"#,
        r#"["HS256"]"#,
        r#""HS256""#,
        "",
    ] {
        assert_eq!(err(&forge(header, BODY)), JwtError::Malformed, "{header}");
    }
}

#[test]
fn a_header_with_crit_is_refused() {
    for header in [
        r#"{"typ":"JWT","alg":"HS256","crit":["exp"]}"#,
        r#"{"typ":"JWT","alg":"HS256","crit":[]}"#,
        r#"{"typ":"JWT","alg":"HS256","crit":null}"#,
    ] {
        assert_eq!(err(&forge(header, BODY)), JwtError::Algorithm, "{header}");
    }
    // Other header fields are not looked at, as jsonwebtoken did not.
    let kid = forge(r#"{"typ":"JWT","alg":"HS256","kid":"1"}"#, BODY);
    assert!(read(&kid, SECRET).is_ok());
}

#[test]
fn exp_may_lie_up_to_the_leeway_in_the_past_and_no_further() {
    assert_eq!(LEEWAY_SECS, 60, "jsonwebtoken's default");
    for exp in [NOW + 3_600, NOW + 1, NOW, NOW - 1, NOW - LEEWAY_SECS] {
        assert!(read(&token(exp), SECRET).is_ok(), "exp {exp}");
    }
    for exp in [NOW - LEEWAY_SECS - 1, NOW - 3_600, 0] {
        assert_eq!(err(&token(exp)), JwtError::Expired, "exp {exp}");
    }
}

#[test]
fn near_the_epoch_the_leeway_does_not_wrap() {
    for now in [0, 30, LEEWAY_SECS] {
        assert!(
            decode_hs256::<AuthClaims>(&token(0), SECRET, now).is_ok(),
            "now {now}"
        );
    }
    assert_eq!(
        decode_hs256::<AuthClaims>(&token(0), SECRET, LEEWAY_SECS + 1).unwrap_err(),
        JwtError::Expired
    );
}

#[test]
fn a_tampered_token_is_refused() {
    let [header, _, signature] = parts(&token(NOW + 3_600));
    let mut engineer = claims(NOW + 3_600);
    engineer.engineer = true;
    let other = B64.encode(serde_json::to_vec(&engineer).unwrap());
    assert_eq!(
        err(&format!("{header}.{other}.{signature}")),
        JwtError::Signature
    );
    let [_, body, _] = parts(&token(NOW + 3_600));
    let typ = B64.encode(r#"{"typ":"jwt","alg":"HS256"}"#);
    assert_eq!(
        err(&format!("{typ}.{body}.{signature}")),
        JwtError::Signature
    );
    let zeros = B64.encode([0u8; 32]);
    assert_eq!(
        err(&format!("{header}.{body}.{zeros}")),
        JwtError::Signature
    );
    // 30 of the 32 bytes: a shorter tag is no tag.
    let short = &signature[..40];
    assert_eq!(
        err(&format!("{header}.{body}.{short}")),
        JwtError::Signature
    );
    let mut flipped = signature.into_bytes();
    flipped[0] = if flipped[0] == b'A' { b'B' } else { b'A' };
    let flipped = String::from_utf8(flipped).unwrap();
    assert_eq!(
        err(&format!("{header}.{body}.{flipped}")),
        JwtError::Signature
    );
}

#[test]
fn a_token_that_is_not_three_base64url_parts_is_malformed() {
    let [h, c, s] = parts(&token(NOW + 3_600));
    for bad in [
        String::new(),
        ".".into(),
        "..".into(),
        h.clone(),
        format!("{h}.{c}"),
        format!("{h}.{c}.{s}="),
        format!("{h}.{c}.{s}!"),
        format!("{h}=.{c}.{s}"),
        format!("x{h}.{c}.{s}"),
    ] {
        assert_eq!(err(&bad), JwtError::Malformed, "{bad}");
    }
    // The signature is looked at before the claims part: signed over, a
    // padded claims part, or a fourth part (a claims part with a dot), is
    // malformed; unsigned, the signature does not verify.
    for input in [format!("{h}.{c}="), format!("{h}.{c}.{s}")] {
        assert_eq!(err(&format!("{input}.{s}")), JwtError::Signature, "{input}");
        let tag = mac(SECRET, &input).unwrap().finalize().into_bytes();
        assert_eq!(err(&signed(input, &tag)), JwtError::Malformed);
    }
}

#[test]
fn claims_without_a_whole_second_exp_or_with_an_audience_are_refused() {
    for body in [
        r#"{"sub":"member1","engineer":false,"iat":1}"#,
        r#"{"sub":"member1","engineer":false,"exp":null,"iat":1}"#,
        r#"{"sub":"member1","engineer":false,"exp":"2000000000","iat":1}"#,
        r#"{"sub":"member1","engineer":false,"exp":2000000000.5,"iat":1}"#,
        r#"{"sub":"member1","engineer":false,"exp":-1,"iat":1}"#,
        r#"{"sub":"member1","engineer":false,"exp":2000000000,"iat":1,"aud":"iemmixer"}"#,
        r#"{"sub":"member1","engineer":false,"exp":2000000000,"iat":1,"aud":[]}"#,
        r#"{"engineer":false,"exp":2000000000,"iat":1}"#,
        r#"{"sub":"member1","engineer":"yes","exp":2000000000,"iat":1}"#,
    ] {
        assert_eq!(err(&forge(HS256_HEADER, body)), JwtError::Claims, "{body}");
    }
    for body in ["[2000000000,null]", "2000000000", "not json"] {
        assert_eq!(
            err(&forge(HS256_HEADER, body)),
            JwtError::Malformed,
            "{body}"
        );
    }
    // `aud: null` is no audience (jsonwebtoken read it so); other claims
    // are the type's business.
    let null_aud =
        r#"{"sub":"member1","engineer":false,"exp":2000000000,"iat":1,"aud":null,"x":1}"#;
    assert!(read(&forge(HS256_HEADER, null_aud), SECRET).is_ok());
}

#[test]
fn a_secret_of_any_length_but_none_signs_and_verifies() {
    // Shorter than, as long as and longer than SHA-256's 64-byte block.
    let secrets: [&[u8]; 4] = [b"k", &[0x5a; 64], &[0xa5; 65], &[1; 200]];
    for secret in secrets {
        let token = encode_hs256(&claims(NOW + 3_600), secret).unwrap();
        assert!(decode_hs256::<AuthClaims>(&token, secret, NOW).is_ok());
        assert_ne!(token, encode_hs256(&claims(NOW + 3_600), SECRET).unwrap());
    }
}

/// Review of #74: an empty key signs nothing and verifies nothing, so a
/// path that forgot to load the secret (a default config's empty
/// `jwt_secret`) never accepts a token anyone could forge.
#[test]
fn an_empty_secret_signs_and_verifies_nothing() {
    assert_eq!(
        encode_hs256(&claims(NOW + 3_600), b""),
        Err(JwtError::Encode)
    );
    let forged = {
        let input = format!(
            "{}.{}",
            B64.encode(HS256_HEADER),
            B64.encode(serde_json::to_vec(&claims(NOW + 3_600)).unwrap())
        );
        let mut m = Hmac::<Sha256>::new_from_slice(b"").unwrap();
        m.update(input.as_bytes());
        signed(input, &m.finalize().into_bytes())
    };
    assert_eq!(
        decode_hs256::<AuthClaims>(&forged, b"", NOW).unwrap_err(),
        JwtError::Signature
    );
}

#[test]
fn claims_that_serde_json_cannot_write_are_an_encode_error() {
    // A map with non-string keys is no JSON object.
    let claims: std::collections::BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into();
    assert_eq!(encode_hs256(&claims, SECRET), Err(JwtError::Encode));
}

#[test]
fn errors_name_no_part_of_the_token() {
    for e in [
        JwtError::Malformed,
        JwtError::Algorithm,
        JwtError::Signature,
        JwtError::Expired,
        JwtError::Claims,
        JwtError::Encode,
    ] {
        let text = e.to_string();
        assert!(text.starts_with("the token"), "{text}");
        assert!(!text.contains("ey"), "{text}");
    }
}

#[test]
fn header_of_reads_any_header_and_refuses_what_is_none() {
    let header = header_of(&forge(r#"{"alg":"HS512","kid":"7"}"#, BODY)).unwrap();
    assert_eq!(
        (header["alg"].as_str(), header["kid"].as_str()),
        (Some("HS512"), Some("7"))
    );
    assert_eq!(header_of("no-dot"), Err(JwtError::Malformed));
    assert_eq!(header_of("!.x.y"), Err(JwtError::Malformed));
}

#[cfg(feature = "vapid")]
mod es256 {
    use super::*;
    use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Verifier};

    /// The VAPID claims as `push::build_vapid_header` writes them: a
    /// `serde_json::Value` whose keys are already in sorted order, so its
    /// bytes are the same whether serde_json keeps insertion order or not.
    fn vapid_claims() -> Value {
        serde_json::json!({
            "aud": "https://push.example.org",
            "exp": 2_000_000_000u64,
            "sub": "mailto:ops@example.org",
        })
    }

    fn key() -> p256::SecretKey {
        p256::SecretKey::from_slice(&[7u8; 32]).unwrap()
    }

    /// The parts of `encode_es256(&vapid_claims(), &key())`: the RFC 6979
    /// P-256/SHA-256 signature of the signing input with the scalar 0x07…07,
    /// computed in Python and verified with OpenSSL.
    const KAT_HEADER: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9";
    const KAT_CLAIMS: &str = "eyJhdWQiOiJodHRwczovL3B1c2guZXhhbXBsZS5vcmciLCJleHAiOjIwMDAwMDAwMDAsInN1YiI6Im1haWx0bzpvcHNAZXhhbXBsZS5vcmcifQ";
    const KAT_SIGNATURE: &str =
        "mkGpFV-Ce-nrADyvP9PFB7NBDArtKzjZ5goJRdWICMPgr1wP1_Jm96jWCUkS42Ww7i5wyj9xVXkY4lV2rmVI4A";

    #[test]
    fn an_es256_token_is_the_rfc_6979_signature_as_r_and_s() {
        let token = encode_es256(&vapid_claims(), &key()).unwrap();
        assert_eq!(token, format!("{KAT_HEADER}.{KAT_CLAIMS}.{KAT_SIGNATURE}"));
        assert_eq!(B64.decode(KAT_HEADER).unwrap(), ES256_HEADER.as_bytes());
        let header = header_of(&token).unwrap();
        assert_eq!(
            (header["typ"].as_str(), header["alg"].as_str()),
            (Some("JWT"), Some("ES256"))
        );
    }

    #[test]
    fn an_es256_signature_verifies_with_the_public_key_only() {
        let token = encode_es256(&vapid_claims(), &key()).unwrap();
        let (input, signature) = token.rsplit_once('.').unwrap();
        let bytes = B64.decode(signature).unwrap();
        assert_eq!(bytes.len(), 64, "r || s");
        let signature = Signature::from_slice(&bytes).unwrap();
        let verifying = VerifyingKey::from(&SigningKey::from(&key()));
        assert!(verifying.verify(input.as_bytes(), &signature).is_ok());
        let other = p256::SecretKey::from_slice(&[8u8; 32]).unwrap();
        let other = VerifyingKey::from(&SigningKey::from(&other));
        assert!(other.verify(input.as_bytes(), &signature).is_err());
        let changed = input.replacen(KAT_CLAIMS, &B64.encode(BODY), 1);
        assert!(verifying.verify(changed.as_bytes(), &signature).is_err());
        let claims: Value = serde_json::from_slice(&B64.decode(KAT_CLAIMS).unwrap()).unwrap();
        assert_eq!(claims, vapid_claims());
    }

    #[test]
    fn an_es256_token_is_no_login_token() {
        let token = encode_es256(&vapid_claims(), &key()).unwrap();
        assert_eq!(
            decode_hs256::<Value>(&token, &key().to_bytes(), 0),
            Err(JwtError::Algorithm)
        );
    }
}
