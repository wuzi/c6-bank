use c6_bank::Client;
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A server requiring a trusted client certificate proves the SDK attaches its PEM identity.
#[tokio::test]
async fn sends_client_certificate_on_auth_and_pix_connections() {
    let mut ca_params = CertificateParams::new(vec![]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let mut server_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server = server_params.signed_by(&server_key, &issuer).unwrap();
    let mut client_params = CertificateParams::new(vec![]).unwrap();
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_key = KeyPair::generate().unwrap();
    let client_cert = client_params.signed_by(&client_key, &issuer).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .unwrap();
    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![server.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected = client_cert.der().clone();
    let server_task = tokio::spawn(async move {
        for response in [
            r#"{"access_token":"token","expires_in":300}"#,
            r#"{"txid":"12345678901234567890123456789012","status":"ATIVA"}"#,
        ] {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            assert_eq!(tls.get_ref().1.peer_certificates().unwrap()[0], expected);
            let mut buf = [0u8; 4096];
            let n = tls.read(&mut buf).await.unwrap();
            assert!(n > 0);
            let message = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            tls.write_all(message.as_bytes()).await.unwrap();
            tls.shutdown().await.unwrap();
        }
    });
    let client = Client::builder("id", "secret")
        .identity_pem(
            client_cert.pem().as_bytes(),
            client_key.serialize_pem().as_bytes(),
        )
        .unwrap()
        .add_root_certificate_pem(ca.pem().as_bytes())
        .unwrap()
        .base_url(&format!("https://localhost:{}", address.port()))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        client
            .get_due_charge("12345678901234567890123456789012")
            .await
            .unwrap()
            .status,
        "ATIVA"
    );
    server_task.await.unwrap();
}

#[test]
fn configuration_and_debug_do_not_disclose_credentials_or_private_keys() {
    let builder = Client::builder("private-client", "private-secret");
    assert!(!format!("{builder:?}").contains("private"));
    assert!(builder.build().is_err());
    let err = Client::builder("id", "secret")
        .identity_pem(b"private-cert", b"private-key")
        .unwrap_err();
    assert!(!format!("{err:?} {err}").contains("private"));
}
