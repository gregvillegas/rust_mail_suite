//! TLS client connections to Dovecot and Postfix (rustls, `ring` provider).

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_rustls::TlsConnector;

use crate::config::{MailConfig, TlsMode};
use crate::error::{MailError, MailResult};

/// How long to wait for a TCP connection or a TLS handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Accepts any certificate. Only ever built for [`TlsMode::Insecure`].
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Builds the TLS connector for the configured verification mode.
///
/// # Errors
/// [`MailError::Tls`] if the CA file cannot be read or holds no certificate.
pub fn connector(config: &MailConfig) -> MailResult<TlsConnector> {
    let provider = Arc::new(ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| MailError::Tls(e.to_string()))?;
    let client_config = match &config.tls {
        TlsMode::Insecure => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
            .with_no_client_auth(),
        TlsMode::Verify { ca_file } => {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(path) = ca_file {
                let mut added = 0;
                for cert in CertificateDer::pem_file_iter(path)
                    .map_err(|e| MailError::Tls(format!("cannot read {}: {e}", path.display())))?
                {
                    let cert = cert.map_err(|e| MailError::Tls(format!("bad certificate in {}: {e}", path.display())))?;
                    roots
                        .add(cert)
                        .map_err(|e| MailError::Tls(format!("unusable certificate in {}: {e}", path.display())))?;
                    added += 1;
                }
                if added == 0 {
                    return Err(MailError::Tls(format!("{} contains no certificate", path.display())));
                }
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok(TlsConnector::from(Arc::new(client_config)))
}

/// Opens a TCP connection with a timeout.
///
/// # Errors
/// [`MailError::Connect`] if it fails or takes too long.
pub async fn tcp(host: &str, port: u16) -> MailResult<TcpStream> {
    timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| MailError::Connect(format!("timed out connecting to {host}:{port}")))?
        .map_err(|e| MailError::Connect(format!("{host}:{port}: {e}")))
}

/// Runs the TLS handshake on an open connection. `host` is the machine we
/// connected to; the certificate is checked against [`MailConfig::server_name`].
///
/// # Errors
/// [`MailError::Tls`] if the name is invalid, the handshake fails, or it times out.
pub async fn upgrade(config: &MailConfig, host: &str, stream: TcpStream) -> MailResult<TlsStream<TcpStream>> {
    let name = ServerName::try_from(config.server_name(host).to_owned())
        .map_err(|_| MailError::Tls("the certificate name is not valid".into()))?;
    timeout(CONNECT_TIMEOUT, connector(config)?.connect(name, stream))
        .await
        .map_err(|_| MailError::Tls("the TLS handshake timed out".into()))?
        .map_err(|e| MailError::Tls(e.to_string()))
}

/// Connects with TLS from the first byte (IMAPS).
///
/// # Errors
/// See [`tcp`] and [`upgrade`].
pub async fn connect(config: &MailConfig, host: &str, port: u16) -> MailResult<TlsStream<TcpStream>> {
    upgrade(config, host, tcp(host, port).await?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(tls: TlsMode) -> MailConfig {
        MailConfig {
            imap_host: "localhost".into(),
            imap_port: 993,
            smtp_host: "localhost".into(),
            smtp_port: 587,
            tls_server_name: None,
            tls,
            master_user: "webmail".into(),
            master_password: "x".into(),
            helo_name: "rustmail.local".into(),
        }
    }

    #[test]
    fn both_modes_build_a_connector() {
        assert!(connector(&config(TlsMode::Insecure)).is_ok());
        assert!(connector(&config(TlsMode::Verify { ca_file: None })).is_ok());
    }

    #[test]
    fn a_missing_or_empty_ca_file_is_an_error_not_a_silent_fallback() {
        let missing = TlsMode::Verify { ca_file: Some("/nonexistent/ca.pem".into()) };
        assert!(connector(&config(missing)).is_err());
        let dir = std::env::temp_dir().join(format!("rustmail-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        let result = connector(&config(TlsMode::Verify { ca_file: Some(empty) }));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_connect_error() {
        let error = tcp("127.0.0.1", 1).await.unwrap_err();
        assert!(matches!(error, MailError::Connect(_)), "{error}");
    }
}
