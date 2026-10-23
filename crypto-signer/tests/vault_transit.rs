//! Firma con Vault Transit contra un Vault simulado que implementa las rutas usadas
//! (`GET /v1/<kv>/data/<ruta>` y `POST /v1/<transit>/sign/<llave>`). El documento firmado
//! se verifica con `xmlsec1`, lo que comprueba la conversión del formato de firma de Vault.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use crypto_signer::provider::{CertificateProvider, VaultTransitProvider};
use crypto_signer::vault::VaultClient;
use crypto_signer::verify::XmlSignatureVerifier;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "s.test-token";
const TEST_KEY_PEM: &str = include_str!("fixtures/test_key.pem");
const TEST_CERT_PEM: &str = include_str!("fixtures/test_cert.pem");

#[derive(Default)]
struct Calls {
    kv: AtomicUsize,
    sign: AtomicUsize,
}

fn signing_key() -> RsaKeyPair {
    let body: String = TEST_KEY_PEM.lines().filter(|l| !l.starts_with("-----")).collect();
    RsaKeyPair::from_pkcs8(&B64.decode(body).unwrap()).unwrap()
}

/// Respuesta del Vault simulado: `(status, cuerpo JSON)`.
fn respond(method: &str, path: &str, token: Option<&str>, body: &str, calls: &Calls) -> (u16, Value) {
    if token != Some(TOKEN) {
        return (403, json!({ "errors": ["permission denied"] }));
    }
    match (method, path) {
        ("GET", "/v1/kv-test/data/tenants/acme") => {
            calls.kv.fetch_add(1, Ordering::SeqCst);
            (200, json!({ "data": { "data": { "certificate": TEST_CERT_PEM, "transit_key": "acme-signing" } } }))
        }
        ("POST", "/v1/transit-test/sign/acme-signing") => {
            calls.sign.fetch_add(1, Ordering::SeqCst);
            let request: Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["hash_algorithm"], "sha2-256");
            assert_eq!(request["signature_algorithm"], "pkcs1v15");
            assert_eq!(request["prehashed"], false);
            let input = B64.decode(request["input"].as_str().unwrap()).unwrap();
            let key = signing_key();
            let mut signature = vec![0u8; key.public().modulus_len()];
            key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), &input, &mut signature).unwrap();
            (200, json!({ "data": { "signature": format!("vault:v3:{}", B64.encode(signature)), "key_version": 3 } }))
        }
        _ => (404, json!({ "errors": [] })),
    }
}

async fn fake_vault() -> (String, Arc<Calls>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Calls::default());
    let shared = calls.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let calls = shared.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                let (head, body) = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&data).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let head = text[..end].to_string();
                        let length = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                            .unwrap_or(0usize);
                        if data.len() >= end + 4 + length {
                            break (head, text[end + 4..end + 4 + length].to_string());
                        }
                    }
                };
                let mut request_line = head.lines().next().unwrap().split(' ');
                let (method, path) = (request_line.next().unwrap(), request_line.next().unwrap());
                let token = head
                    .lines()
                    .find_map(|l| l.strip_prefix("x-vault-token: ").or_else(|| l.strip_prefix("X-Vault-Token: ")));
                let (status, json) = respond(method, path, token, &body, &calls);
                let payload = json.to_string();
                let response = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    payload.len(),
                    payload
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (address, calls)
}

fn provider(address: String, token: &str) -> VaultTransitProvider {
    VaultTransitProvider::new(
        VaultClient::new(address, token.to_string())
            .unwrap()
            .with_mounts("kv-test", "transit-test")
            .unwrap(),
    )
}

#[tokio::test]
async fn documents_signed_in_transit_verify_with_xmlsec1() {
    let (address, calls) = fake_vault().await;
    let provider = provider(address, TOKEN);
    let xml = r#"<Invoice xmlns="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2"><UBLExtensions><!-- PLACEHOLDER_FOR_SIGNATURE --></UBLExtensions><ID>FE1</ID></Invoice>"#;

    let first = crypto_signer::sign_document(xml, &provider, "tenants/acme").await.unwrap();
    let second = crypto_signer::sign_document(xml, &provider, "tenants/acme").await.unwrap();

    assert_eq!(calls.sign.load(Ordering::SeqCst), 2, "the private key is used only inside Vault");
    assert_eq!(calls.kv.load(Ordering::SeqCst), 1, "certificate and key name are cached");
    if std::process::Command::new("xmlsec1").arg("--version").output().is_err() {
        assert!(std::env::var_os("CI").is_none(), "xmlsec1 is required in CI");
        eprintln!("xmlsec1 not installed: skipping verification of Transit signatures");
        return;
    }
    let verifier = XmlSignatureVerifier::new(format!("{}/tests/fixtures/test_ca.pem", env!("CARGO_MANIFEST_DIR")));
    verifier.verify(&first).unwrap();
    verifier.verify(&second).unwrap();
}

#[tokio::test]
async fn vault_errors_and_unsafe_references_are_reported() {
    let (address, _) = fake_vault().await;
    let denied = provider(address.clone(), "wrong-token");
    let error = denied.sign_rsa_sha256("tenants/acme", b"x").await.unwrap_err().to_string();
    assert!(error.contains("403"), "{}", error);

    let allowed = provider(address, TOKEN);
    assert!(allowed.get_certificate("tenants/missing").await.unwrap_err().to_string().contains("404"));
    assert!(allowed.get_certificate("../sys/policy/root").await.is_err());
}
