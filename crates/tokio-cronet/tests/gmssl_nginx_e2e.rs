#![cfg(feature = "gmssl-tests")]

use std::{env, fs, path::PathBuf, time::Duration};

use gmssl_rs::{Sm3, X509Cert};
use tokio_cronet::{GmSslClient, GmSslProtocol, Request};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nginx_gmssl_tls12_tls13_and_tlcp_interoperate() {
    let certificate_root = PathBuf::from(
        env::var_os("GMSSL_E2E_CERTIFICATE_ROOT")
            .expect("GMSSL_E2E_CERTIFICATE_ROOT must point to nginx-gmssl TLCP certificates"),
    );
    let ca = certificate_root.join("rootcacert.pem");
    let leaf = certificate_root.join("signcert.pem");
    let leaf_pin = Sm3::digest(
        X509Cert::from_pem(&fs::read(&leaf).unwrap())
            .unwrap()
            .as_der(),
    );
    let client_certificate = PathBuf::from(
        env::var_os("GMSSL_E2E_CLIENT_CERTIFICATE")
            .expect("GMSSL_E2E_CLIENT_CERTIFICATE must point to an SM2 certificate chain"),
    );
    let client_private_key = PathBuf::from(
        env::var_os("GMSSL_E2E_CLIENT_PRIVATE_KEY")
            .expect("GMSSL_E2E_CLIENT_PRIVATE_KEY must point to an SM2 private key"),
    );
    let client_key_password = env::var("GMSSL_E2E_CLIENT_KEY_PASSWORD")
        .expect("GMSSL_E2E_CLIENT_KEY_PASSWORD must contain the SM2 key password");
    let cases = [
        (GmSslProtocol::Tls12, 8443),
        (GmSslProtocol::Tls13, 8444),
        (GmSslProtocol::Tlcp, 8445),
    ];

    for (protocol, port) in cases {
        let builder = GmSslClient::builder()
            .protocol(protocol)
            .ca_certificates(&ca)
            .client_identity(
                &client_certificate,
                &client_private_key,
                &client_key_password,
            )
            .connect_timeout(Duration::from_secs(5))
            .io_timeout(Duration::from_secs(10))
            .verify_depth(4);
        let builder = if protocol == GmSslProtocol::Tls13 {
            builder.server_certificate_sm3(leaf_pin)
        } else {
            builder.server_certificate(&leaf).unwrap()
        };
        let client = builder.build().unwrap();
        let request = Request::builder(format!("https://localhost:{port}/"))
            .unwrap()
            .header("accept", "text/html")
            .unwrap()
            .max_response_bytes(4096)
            .build()
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(15), client.execute(request))
            .await
            .unwrap_or_else(|_| panic!("{protocol:?} request timed out"))
            .unwrap_or_else(|error| panic!("{protocol:?} request failed: {error}"));
        assert_eq!(response.protocol, protocol);
        assert_eq!(response.status(), 200);
        assert_eq!(response.body(), b"gmssl nginx ok\n");
        assert!(!response.headers.is_empty());
        assert_eq!(response.clone().into_body(), response.body());
    }
}
