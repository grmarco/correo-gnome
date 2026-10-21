use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Par PKCE (RFC 7636) con método S256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Genera un verificador aleatorio de 43 caracteres (32 bytes en base64url).
    pub fn generate() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
        let challenge = s256_challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }
}

/// `BASE64URL(SHA256(verifier))` sin padding.
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Valor `state` anti-CSRF aleatorio (32 bytes en base64url).
pub fn random_state() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rng().fill(&mut buf[..]);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc7636_vector() {
        // Apéndice B de la RFC 7636.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_valid_and_consistent() {
        let pkce = Pkce::generate();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(pkce.verifier.bytes().all(|b| b.is_ascii_alphanumeric()
            || b == b'-'
            || b == b'.'
            || b == b'_'
            || b == b'~'));
        assert_eq!(pkce.challenge, s256_challenge(&pkce.verifier));
        assert_ne!(pkce.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn state_is_random_and_url_safe() {
        let a = random_state();
        let b = random_state();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }
}
