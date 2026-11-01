//! Password pairing for cloud servers. The server keeps a PBKDF2 verifier, never
//! the password. Both sides prove they know it with HMACs bound to the TLS
//! certificate the client actually received, so a client that typed only an
//! address and password still pins the genuine server (see docs/cloud-agents.md).
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    hmac, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

pub const ITERATIONS: u32 = 600_000;
pub const MINIMUM_LENGTH: usize = 12;
const KEY_BYTES: usize = 32;
const LABEL: &str = "difu-pair-v1";

/// What the server stores for its password: salt, work factor and derived key,
/// each base64url without padding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verifier {
    pub salt: String,
    pub iterations: u32,
    pub key: String,
}

impl Verifier {
    pub fn create(password: &str) -> Result<Self> {
        Self::with_iterations(password, ITERATIONS)
    }

    pub(crate) fn with_iterations(password: &str, iterations: u32) -> Result<Self> {
        ensure!(
            password.chars().count() >= MINIMUM_LENGTH,
            "The server password needs at least {MINIMUM_LENGTH} characters"
        );
        let mut salt = [0u8; 16];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
        let key = derive(password, &salt, iterations)?;
        Ok(Self {
            salt: URL_SAFE_NO_PAD.encode(salt),
            iterations,
            key: URL_SAFE_NO_PAD.encode(key),
        })
    }

    fn key(&self) -> Result<Vec<u8>> {
        let key = URL_SAFE_NO_PAD
            .decode(&self.key)
            .context("Invalid stored password key")?;
        ensure!(key.len() == KEY_BYTES, "Invalid stored password key");
        Ok(key)
    }

    /// Checks a client's proof against this server's own certificate fingerprint.
    pub fn accepts(
        &self,
        fingerprint: &str,
        server_nonce: &str,
        client_nonce: &str,
        proof: &str,
    ) -> bool {
        let (Ok(key), Ok(proof)) = (self.key(), URL_SAFE_NO_PAD.decode(proof)) else {
            return false;
        };
        let message = message("client", fingerprint, server_nonce, client_nonce);
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, &key),
            message.as_bytes(),
            &proof,
        )
        .is_ok()
    }

    /// The server's answer, which the client checks before trusting the server.
    pub fn server_proof(
        &self,
        fingerprint: &str,
        server_nonce: &str,
        client_nonce: &str,
    ) -> Result<String> {
        Ok(sign(
            &self.key()?,
            "server",
            fingerprint,
            server_nonce,
            client_nonce,
        ))
    }
}

/// `PBKDF2-HMAC-SHA256(password, salt, iterations)`, 32 bytes.
pub fn derive(password: &str, salt: &[u8], iterations: u32) -> Result<[u8; KEY_BYTES]> {
    let iterations = NonZeroU32::new(iterations).context("Invalid password work factor")?;
    let mut key = [0u8; KEY_BYTES];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut key,
    );
    Ok(key)
}

/// Client side of the exchange: derive the key from the server's challenge,
/// prove it, and later check the server's proof.
pub struct ClientKey([u8; KEY_BYTES]);

impl ClientKey {
    pub fn derive(password: &str, salt: &str, iterations: u32) -> Result<Self> {
        ensure!(
            (100_000..=10_000_000).contains(&iterations),
            "The server asked for an unusual password work factor"
        );
        let salt = URL_SAFE_NO_PAD
            .decode(salt)
            .context("Invalid password salt from the server")?;
        Ok(Self(derive(password, &salt, iterations)?))
    }

    pub fn proof(&self, fingerprint: &str, server_nonce: &str, client_nonce: &str) -> String {
        sign(&self.0, "client", fingerprint, server_nonce, client_nonce)
    }

    pub fn verify_server(
        &self,
        fingerprint: &str,
        server_nonce: &str,
        client_nonce: &str,
        proof: &str,
    ) -> bool {
        let Ok(proof) = URL_SAFE_NO_PAD.decode(proof) else {
            return false;
        };
        let message = message("server", fingerprint, server_nonce, client_nonce);
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, &self.0),
            message.as_bytes(),
            &proof,
        )
        .is_ok()
    }
}

/// Base64url of 32 random bytes, for the `nonce` fields.
pub fn nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// A strong password that is still easy to type on a phone: five groups of five
/// lowercase letters and digits (about 129 bits), e.g. `k3v9q-...`.
pub fn generate() -> Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 25];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
    let mut password = String::with_capacity(29);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && index % 5 == 0 {
            password.push('-');
        }
        let symbol = ALPHABET
            .get(usize::from(*byte) % ALPHABET.len())
            .copied()
            .context("Password alphabet")?;
        password.push(char::from(symbol));
    }
    Ok(password)
}

fn message(role: &str, fingerprint: &str, server_nonce: &str, client_nonce: &str) -> String {
    format!("{LABEL}|{role}|{fingerprint}|{server_nonce}|{client_nonce}")
}

fn sign(
    key: &[u8],
    role: &str,
    fingerprint: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> String {
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, key),
        message(role, fingerprint, server_nonce, client_nonce).as_bytes(),
    );
    URL_SAFE_NO_PAD.encode(tag.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_passwords_prove_both_sides_for_the_same_certificate() -> Result<()> {
        let verifier = Verifier::with_iterations("correct horse battery", 100_000)?;
        let (sn, cn) = (nonce()?, nonce()?);
        let client = ClientKey::derive("correct horse battery", &verifier.salt, 100_000)?;
        let proof = client.proof("server-cert", &sn, &cn);
        assert!(verifier.accepts("server-cert", &sn, &cn, &proof));
        let answer = verifier.server_proof("server-cert", &sn, &cn)?;
        assert!(client.verify_server("server-cert", &sn, &cn, &answer));
        Ok(())
    }

    #[test]
    fn wrong_password_or_other_certificate_is_rejected() -> Result<()> {
        let verifier = Verifier::with_iterations("correct horse battery", 100_000)?;
        let (sn, cn) = (nonce()?, nonce()?);
        let wrong = ClientKey::derive("correct horse battery!", &verifier.salt, 100_000)?;
        assert!(!verifier.accepts(
            "server-cert",
            &sn,
            &cn,
            &wrong.proof("server-cert", &sn, &cn)
        ));
        // A man in the middle presents its own certificate to the client.
        let client = ClientKey::derive("correct horse battery", &verifier.salt, 100_000)?;
        let relayed = client.proof("attacker-cert", &sn, &cn);
        assert!(!verifier.accepts("server-cert", &sn, &cn, &relayed));
        let answer = verifier.server_proof("server-cert", &sn, &cn)?;
        assert!(!client.verify_server("attacker-cert", &sn, &cn, &answer));
        assert!(!client.verify_server("server-cert", &sn, &cn, "not base64!"));
        Ok(())
    }

    #[test]
    fn short_passwords_and_odd_work_factors_are_refused() -> Result<()> {
        assert!(Verifier::create("short").is_err());
        let verifier = Verifier::with_iterations("long enough password", 100_000)?;
        assert!(ClientKey::derive("long enough password", &verifier.salt, 10).is_err());
        Ok(())
    }

    #[test]
    fn known_vector_matches_other_implementations() -> Result<()> {
        // The iOS client (DifuKit) tests against the same vector.
        let client = ClientKey::derive("difu test password", "MDEyMzQ1Njc4OWFiY2RlZg", 100_000)?;
        assert_eq!(
            URL_SAFE_NO_PAD.encode(client.0),
            "ZuRZLJbLiD4kaB4n6BBY2rNnWNmwex3mIuH_9tp147Q"
        );
        assert_eq!(
            client.proof("fingerprint", "server-nonce", "client-nonce"),
            "94cIXRPlcWBtrQv3mKJ3qtvZsXrjN6GvnJjZkYvpw2w"
        );
        let verifier = Verifier {
            salt: "MDEyMzQ1Njc4OWFiY2RlZg".into(),
            iterations: 100_000,
            key: "ZuRZLJbLiD4kaB4n6BBY2rNnWNmwex3mIuH_9tp147Q".into(),
        };
        assert_eq!(
            verifier.server_proof("fingerprint", "server-nonce", "client-nonce")?,
            "WfFgJvmNYXPD06hqmuyAXT2AHIAgLPA5rLTdVlDFuto"
        );
        Ok(())
    }

    #[test]
    fn generated_passwords_are_long_and_grouped() -> Result<()> {
        let password = generate()?;
        assert_eq!(password.len(), 29);
        assert_eq!(password.matches('-').count(), 4);
        assert_ne!(password, generate()?);
        assert!(Verifier::with_iterations(&password, 100_000).is_ok());
        Ok(())
    }
}
