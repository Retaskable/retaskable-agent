//! The TLS half of the `probes` task: one handshake with the site, reading what the certificate says (`tls()` in
//! `securityProbes.ts`). rustls offers TLS 1.2 and 1.3 individually; it cannot offer TLS 1.0 or 1.1, so those answer
//! a local failure (`unknown` in `tls-versions`, never a guess).
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::probes::{TlsAnswer, TlsVersion};

/// Connect plus handshake.
const TIMEOUT: Duration = Duration::from_secs(10);

fn local(error: impl Into<String>) -> TlsAnswer {
    TlsAnswer::Failed {
        local: true,
        error: error.into(),
    }
}

/// Judges the chain against the public roots but never fails the handshake over it: this probe is read-only and
/// sends no data, and the certificate check has to read the expiry of an expired, self-signed or otherwise untrusted
/// certificate to report on it. The verdict is kept in `verdict` (`None` = trusted) for the answer's `authorized`.
#[derive(Debug)]
struct RecordingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    verdict: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let r = self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
        *self.verdict.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = r.err().map(|e| format!("{e:?}"));
        Ok(ServerCertVerified::assertion())
    }

    // The handshake's own signatures are still checked, so what we read is what the server holds.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn valid_to(leaf: &CertificateDer<'_>) -> Option<DateTime<Utc>> {
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref()).ok()?;
    DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0)
}

/// A handshake with `host:port`, allowing only `version` when given (TLS 1.0/1.1 can't be offered: local failure).
pub(super) async fn handshake(host: &str, port: u16, version: Option<TlsVersion>) -> TlsAnswer {
    let pinned: &[&rustls::SupportedProtocolVersion] = match version {
        None => rustls::ALL_VERSIONS,
        Some(v) => return local(format!("{} can't be offered by this agent's TLS library", v.name())),
    };
    handshake_with(host, port, pinned).await
}

/// Test seam: `versions` is what the client offers (TLS 1.2 and 1.3 individually).
pub(super) async fn handshake_with(
    host: &str,
    port: u16,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> TlsAnswer {
    let Ok(name) = ServerName::try_from(host.to_string()) else {
        return local(format!("{host} isn't a valid host name"));
    };
    let provider: Arc<CryptoProvider> = Arc::new(ring::default_provider());
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let inner = match WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build() {
        Ok(v) => v,
        Err(e) => return local(format!("couldn't set up certificate checks: {e}")),
    };
    let verdict = Arc::new(Mutex::new(None));
    let config = match ClientConfig::builder_with_provider(provider).with_protocol_versions(versions) {
        Ok(b) => b,
        Err(e) => return local(format!("couldn't set up TLS: {e}")),
    }
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(RecordingVerifier {
        inner,
        verdict: verdict.clone(),
    }))
    .with_no_client_auth();

    let attempt = async {
        let tcp = TcpStream::connect((host, port)).await.map_err(|e| e.to_string())?;
        TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|e| e.to_string())
    };
    let stream = match tokio::time::timeout(TIMEOUT, attempt).await {
        Err(_) => {
            return TlsAnswer::Failed {
                local: false,
                error: format!("timed out after {}s", TIMEOUT.as_secs()),
            };
        }
        Ok(Err(error)) => return TlsAnswer::Failed { local: false, error },
        Ok(Ok(s)) => s,
    };
    let (_, conn) = stream.get_ref();
    let protocol = conn
        .protocol_version()
        .map_or_else(|| "unknown".to_string(), |v| format!("{v:?}").replace('_', "."));
    let leaf = conn.peer_certificates().and_then(|c| c.first());
    let auth_error = verdict
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    TlsAnswer::Ok {
        protocol,
        authorized: leaf.is_some() && auth_error.is_none(),
        auth_error,
        valid_to: leaf.and_then(valid_to),
    }
}

#[cfg(test)]
mod tests;
