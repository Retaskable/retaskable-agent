use std::sync::Arc;

use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, version};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::*;

/// A loopback TLS server with a fresh self-signed certificate expiring at `not_after` (year, month, day).
async fn serve(versions: &[&'static rustls::SupportedProtocolVersion]) -> u16 {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params.not_after = rcgen::date_time_ymd(2041, 3, 9);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let _ = acceptor.accept(tcp).await;
            });
        }
    });
    port
}

#[tokio::test]
async fn reads_the_expiry_of_an_untrusted_certificate() {
    let port = serve(rustls::ALL_VERSIONS).await;
    let TlsAnswer::Ok {
        protocol,
        authorized,
        auth_error,
        valid_to,
    } = handshake("localhost", port, None).await
    else {
        panic!("handshake failed");
    };
    assert_eq!(protocol, "TLSv1.3");
    assert!(!authorized);
    assert!(auth_error.is_some());
    assert_eq!(valid_to.unwrap().to_rfc3339(), "2041-03-09T00:00:00+00:00");
}

#[tokio::test]
async fn offers_1_2_and_1_3_individually() {
    let port = serve(rustls::ALL_VERSIONS).await;
    for (v, name) in [(&version::TLS12, "TLSv1.2"), (&version::TLS13, "TLSv1.3")] {
        match handshake_with("localhost", port, &[v]).await {
            TlsAnswer::Ok { protocol, .. } => assert_eq!(protocol, name),
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_1_2_only_server_refuses_1_3() {
    let port = serve(&[&version::TLS12]).await;
    let a = handshake_with("localhost", port, &[&version::TLS13]).await;
    assert!(matches!(a, TlsAnswer::Failed { local: false, .. }), "{a:?}");
    let a = handshake("localhost", port, None).await;
    assert!(
        matches!(a, TlsAnswer::Ok { ref protocol, .. } if protocol == "TLSv1.2"),
        "{a:?}"
    );
}

#[tokio::test]
async fn old_versions_stay_unknown() {
    let port = serve(rustls::ALL_VERSIONS).await;
    for v in [TlsVersion::V1_0, TlsVersion::V1_1] {
        let a = handshake("localhost", port, Some(v)).await;
        assert!(matches!(a, TlsAnswer::Failed { local: true, .. }), "{a:?}");
    }
}

#[tokio::test]
async fn a_refused_connection_is_a_failure() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let a = handshake("127.0.0.1", port, None).await;
    assert!(matches!(a, TlsAnswer::Failed { local: false, .. }), "{a:?}");
}
