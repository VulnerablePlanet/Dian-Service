//! Proveedores de certificado y de firma RSA PKCS#1 v1.5 con SHA-256.
//!
//! - [`VaultTransitProvider`] (producción): la llave privada vive en Vault Transit y nunca
//!   sale de él; el proceso solo envía los datos a firmar. El certificado se lee de KV v2.
//! - [`LocalCertificateProvider`]: par PEM en memoria, para desarrollo y pruebas.
//! - [`VaultKvKeyProvider`] (legado): lee la llave privada de KV v2 y firma en el proceso.
//!   Se conserva solo para migrar instalaciones existentes.
//!
//! Las firmas en proceso usan `ring` (aritmética de tiempo constante).

use crate::error::CryptoError;
use crate::vault::VaultClient;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use x509_cert::der::Decode;

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    /// Certificado X.509 del firmante: DER codificado en Base64 (sin cabeceras PEM).
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError>;

    /// Firma RSA PKCS#1 v1.5 con SHA-256 de `data` (el proveedor calcula el hash).
    async fn sign_rsa_sha256(&self, key_ref: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

/// Contenido Base64 del bloque PEM (sin cabeceras ni saltos de línea).
fn pem_body(pem: &str) -> String {
    pem.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect()
}

/// Valida que el PEM sea un certificado X.509 y devuelve su DER en Base64.
pub fn certificate_from_pem(pem: &str) -> Result<String, CryptoError> {
    let body = pem_body(pem);
    let der = B64
        .decode(&body)
        .map_err(|e| CryptoError::Provider(format!("Invalid certificate PEM: {}", e)))?;
    x509_cert::Certificate::from_der(&der)
        .map_err(|e| CryptoError::Provider(format!("Invalid X.509 certificate: {}", e)))?;
    Ok(body)
}

/// Llave privada RSA en PEM PKCS#8 (`PRIVATE KEY`) o PKCS#1 (`RSA PRIVATE KEY`).
fn parse_private_key(pem: &str) -> Result<RsaKeyPair, CryptoError> {
    let der = B64
        .decode(pem_body(pem))
        .map_err(|e| CryptoError::Provider(format!("Invalid private key PEM: {}", e)))?;
    let parsed = if pem.contains("BEGIN RSA PRIVATE KEY") {
        RsaKeyPair::from_der(&der)
    } else {
        RsaKeyPair::from_pkcs8(&der)
    };
    parsed.map_err(|e| CryptoError::Provider(format!("Unsupported RSA private key: {}", e)))
}

fn sign_in_process(key: &RsaKeyPair, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), data, &mut signature)
        .map_err(|_| CryptoError::Signing("RSA signing failed".to_string()))?;
    Ok(signature)
}

/// Par local en memoria (desarrollo y pruebas).
pub struct LocalCertificateProvider {
    certificate: String,
    key: RsaKeyPair,
}

impl LocalCertificateProvider {
    /// Valida de inmediato el certificado y la llave, para fallar al arrancar y no en el
    /// primer documento.
    pub fn from_pem(certificate_pem: String, private_key_pem: String) -> Result<Self, CryptoError> {
        Ok(Self {
            certificate: certificate_from_pem(&certificate_pem)?,
            key: parse_private_key(&private_key_pem)?,
        })
    }
}

#[async_trait]
impl CertificateProvider for LocalCertificateProvider {
    async fn get_certificate(&self, _key_ref: &str) -> Result<String, CryptoError> {
        Ok(self.certificate.clone())
    }

    async fn sign_rsa_sha256(&self, _key_ref: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        sign_in_process(&self.key, data)
    }
}

fn secret_field<'a>(secret: &'a serde_json::Value, field: &str) -> Result<&'a str, CryptoError> {
    secret
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| CryptoError::Provider(format!("Field '{}' not found in Vault secret", field)))
}

/// Legado: certificado y llave privada en el mismo secreto KV v2 (`certificate`,
/// `private_key`). La llave se descarga y se usa en el proceso.
pub struct VaultKvKeyProvider {
    client: VaultClient,
}

impl VaultKvKeyProvider {
    pub fn new(client: VaultClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CertificateProvider for VaultKvKeyProvider {
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError> {
        certificate_from_pem(secret_field(&self.client.read_secret(key_ref).await?, "certificate")?)
    }

    async fn sign_rsa_sha256(&self, key_ref: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let secret = self.client.read_secret(key_ref).await?;
        sign_in_process(&parse_private_key(secret_field(&secret, "private_key")?)?, data)
    }
}

/// Datos de firma de un tenant guardados en KV v2.
#[derive(Clone)]
struct TransitIdentity {
    certificate: String,
    transit_key: String,
}

/// Cuánto se reutiliza el secreto KV de un tenant antes de volver a leerlo (rotaciones).
const IDENTITY_TTL: Duration = Duration::from_secs(300);

/// Producción: firma con una llave RSA no exportable de Vault Transit.
///
/// `key_ref` es la ruta KV v2 (`certificates.vault_reference_path`) de un secreto con
/// `certificate` (PEM) y `transit_key` (nombre de la llave en Transit).
pub struct VaultTransitProvider {
    client: VaultClient,
    cache: Mutex<HashMap<String, (Instant, TransitIdentity)>>,
}

impl VaultTransitProvider {
    pub fn new(client: VaultClient) -> Self {
        Self {
            client,
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn identity(&self, key_ref: &str) -> Result<TransitIdentity, CryptoError> {
        if let Some((fetched, identity)) = self.cache.lock().expect("cache lock").get(key_ref) {
            if fetched.elapsed() < IDENTITY_TTL {
                return Ok(identity.clone());
            }
        }
        let secret = self.client.read_secret(key_ref).await?;
        let identity = TransitIdentity {
            certificate: certificate_from_pem(secret_field(&secret, "certificate")?)?,
            transit_key: secret_field(&secret, "transit_key")?.to_string(),
        };
        self.cache
            .lock()
            .expect("cache lock")
            .insert(key_ref.to_string(), (Instant::now(), identity.clone()));
        Ok(identity)
    }
}

#[async_trait]
impl CertificateProvider for VaultTransitProvider {
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError> {
        Ok(self.identity(key_ref).await?.certificate)
    }

    async fn sign_rsa_sha256(&self, key_ref: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let identity = self.identity(key_ref).await?;
        self.client.transit_sign_pkcs1_sha256(&identity.transit_key, data).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY_PEM: &str = include_str!("../tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../tests/fixtures/test_cert.pem");

    #[test]
    fn local_provider_validates_its_inputs() {
        assert!(LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).is_ok());
        assert!(LocalCertificateProvider::from_pem("cert_dummy".into(), TEST_KEY_PEM.into()).is_err());
        assert!(LocalCertificateProvider::from_pem(TEST_KEY_PEM.into(), TEST_KEY_PEM.into()).is_err());
        assert!(LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_CERT_PEM.into()).is_err());
    }

    /// La firma de `ring` es PKCS#1 v1.5 estándar: OpenSSL la verifica con la llave pública
    /// del certificado.
    #[tokio::test]
    async fn signatures_verify_with_openssl() {
        if std::process::Command::new("openssl").arg("version").output().is_err() {
            return;
        }
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        let signature = provider.sign_rsa_sha256("local", b"datos firmados").await.unwrap();
        let dir = std::env::temp_dir().join(format!("provider-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("data"), b"datos firmados").unwrap();
        std::fs::write(dir.join("sig"), &signature).unwrap();
        std::fs::write(dir.join("cert.pem"), TEST_CERT_PEM).unwrap();
        let pubkey = std::process::Command::new("openssl")
            .args(["x509", "-pubkey", "-noout", "-in"])
            .arg(dir.join("cert.pem"))
            .output()
            .unwrap();
        std::fs::write(dir.join("pub.pem"), pubkey.stdout).unwrap();
        let verified = std::process::Command::new("openssl")
            .args(["dgst", "-sha256", "-verify"])
            .arg(dir.join("pub.pem"))
            .arg("-signature")
            .arg(dir.join("sig"))
            .arg(dir.join("data"))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(verified.status.success(), "{}", String::from_utf8_lossy(&verified.stdout));
    }
}
