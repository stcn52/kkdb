// ── R29: TLS Configuration Module ────────────────────────────────────────────
//
// Provides optional TLS encryption for the MySQL wire protocol.
// Activated via environment variables:
//   KKDB_TLS_CERT — path to PEM-encoded certificate chain
//   KKDB_TLS_KEY  — path to PEM-encoded private key
//
// When both are set, the MySQL server wraps each accepted TCP connection
// with a TLS layer using rustls (no OpenSSL dependency).
//
// Usage:
//   export KKDB_TLS_CERT=/path/to/cert.pem
//   export KKDB_TLS_KEY=/path/to/key.pem
//   kkdb --server

use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::rustls::{self, pki_types::PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// TLS configuration loaded from PEM files.
pub struct TlsConfig {
    pub acceptor: TlsAcceptor,
}

impl TlsConfig {
    /// Load TLS certificate and key from PEM files.
    ///
    /// Returns `None` if the environment variables are not set.
    /// Returns `Err` if the files cannot be read or parsed.
    pub fn from_env() -> io::Result<Option<Self>> {
        let cert_path = match std::env::var("KKDB_TLS_CERT") {
            Ok(p) if !p.is_empty() => p,
            _ => return Ok(None),
        };
        let key_path = match std::env::var("KKDB_TLS_KEY") {
            Ok(p) if !p.is_empty() => p,
            _ => return Ok(None),
        };

        Self::from_files(&cert_path, &key_path).map(Some)
    }

    /// Load TLS certificate and key from specific file paths.
    pub fn from_files(cert_path: &str, key_path: &str) -> io::Result<Self> {
        let certs = load_certs(Path::new(cert_path))?;
        let key = load_private_key(Path::new(key_path))?;

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(TlsConfig {
            acceptor: TlsAcceptor::from(Arc::new(config)),
        })
    }
}

/// Load PEM-encoded certificates from a file.
fn load_certs(
    path: &Path,
) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("failed to open cert file {:?}: {}", path, e)))?;
    let mut reader = io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid cert: {e}")))
}

/// Load the first PEM-encoded private key from a file.
fn load_private_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("failed to open key file {:?}: {}", path, e)))?;
    let mut reader = io::BufReader::new(file);

    // Try PKCS#8 first, then RSA, then EC
    loop {
        match rustls_pemfile::read_one(&mut reader)? {
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs8(key));
            }
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs1(key));
            }
            Some(rustls_pemfile::Item::Sec1Key(key)) => {
                return Ok(PrivateKeyDer::Sec1(key));
            }
            Some(_) => continue, // skip other PEM items
            None => break,
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("no private key found in {:?}", path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_from_env_returns_none_when_unset() {
        // Clear env vars to test the None path
        std::env::remove_var("KKDB_TLS_CERT");
        std::env::remove_var("KKDB_TLS_KEY");
        let result = TlsConfig::from_env().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn tls_config_from_files_errors_on_missing_file() {
        let result = TlsConfig::from_files("/nonexistent/cert.pem", "/nonexistent/key.pem");
        assert!(result.is_err());
    }

    #[test]
    fn load_certs_errors_on_missing_file() {
        let result = load_certs(Path::new("/nonexistent.pem"));
        assert!(result.is_err());
    }

    #[test]
    fn load_private_key_errors_on_missing_file() {
        let result = load_private_key(Path::new("/nonexistent.pem"));
        assert!(result.is_err());
    }
}
