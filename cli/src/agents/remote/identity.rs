//! The Mac's self-signed TLS identity. Phones pin the certificate's SHA-256, so
//! the certificate is created once and kept; deleting it requires re-pairing.
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path, sync::Arc};

pub struct Identity {
    pub fingerprint: String,
    pub tls: Arc<rustls::ServerConfig>,
}

impl Identity {
    pub fn load_or_create(home: &Path) -> Result<Self> {
        super::private_dir(home)?;
        let cert_path = home.join("certificate.der");
        let key_path = home.join("key.der");
        let (cert, key) = match (fs::read(&cert_path), fs::read(&key_path)) {
            (Ok(cert), Ok(key)) => (cert, key),
            _ => {
                let mut params = rcgen::CertificateParams::new(vec!["difu.local".to_owned()])
                    .context("Cannot describe the remote control certificate")?;
                params
                    .distinguished_name
                    .push(rcgen::DnType::CommonName, "difu remote control");
                params.not_before = rcgen::date_time_ymd(2025, 1, 1);
                params.not_after = rcgen::date_time_ymd(2045, 1, 1);
                let key =
                    rcgen::KeyPair::generate().context("Cannot create the remote control key")?;
                let cert = params
                    .self_signed(&key)
                    .context("Cannot create the remote control certificate")?;
                let (cert, key) = (cert.der().to_vec(), key.serialize_der());
                // The key is written first: a certificate without its key is useless.
                write_private(&key_path, &key)?;
                write_private(&cert_path, &cert)?;
                (cert, key)
            }
        };
        let fingerprint = URL_SAFE_NO_PAD.encode(Sha256::digest(&cert));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("Cannot configure TLS")?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .context("The remote control certificate is damaged; delete the remote folder and pair again")?;
        Ok(Self {
            fingerprint,
            tls: Arc::new(tls),
        })
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// URL-safe random secret with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> Result<String> {
    let mut buffer = vec![0; bytes];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut buffer)
        .map_err(|_| anyhow::anyhow!("The system random number generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// Constant-time comparison for secrets and secret hashes.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_created_once_and_reused() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().join("remote");
        let first = Identity::load_or_create(&home)?;
        let second = Identity::load_or_create(&home)?;
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.fingerprint.len(), 43);
        let mode = fs::metadata(home.join("key.der"))?.permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
        Ok(())
    }

    #[test]
    fn tokens_are_random_and_comparison_is_exact() -> Result<()> {
        let a = random_token(32)?;
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_token(32)?);
        assert!(same(&a, &a.clone()));
        assert!(!same(&a, "short"));
        Ok(())
    }
}
