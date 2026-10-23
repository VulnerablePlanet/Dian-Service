use crate::error::CryptoError;
use crate::vault::VaultClient;
use async_trait::async_trait;
use rsa::{pkcs1::DecodeRsaPrivateKey, pkcs8::DecodePrivateKey, RsaPrivateKey};
use rsa::Pkcs1v15Sign;
use sha2::Sha256;

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    /// Obtiene el certificado digital en formato PEM (codificado en Base64).
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError>;

    /// Firma (RSA PKCS#1 v1.5) un digest SHA-256 ya calculado.
    async fn sign_sha256(&self, key_ref: &str, digest: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

/// Proveedor local para desarrollo/tests que almacena las llaves en memoria
pub struct LocalCertificateProvider {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl LocalCertificateProvider {
    /// Construye el proveedor validando de inmediato que la llave y el certificado se
    /// pueden usar, para fallar al arrancar y no en el primer documento.
    pub fn from_pem(certificate_pem: String, private_key_pem: String) -> Result<Self, CryptoError> {
        parse_pem_private_key(&private_key_pem)?;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, clean_pem(&certificate_pem))
            .map_err(|e| CryptoError::Provider(format!("Invalid certificate PEM: {}", e)))?;
        Ok(Self {
            certificate_pem,
            private_key_pem,
        })
    }
}

#[async_trait]
impl CertificateProvider for LocalCertificateProvider {
    async fn get_certificate(&self, _key_ref: &str) -> Result<String, CryptoError> {
        Ok(clean_pem(&self.certificate_pem))
    }

    async fn sign_sha256(&self, _key_ref: &str, digest: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let private_key = parse_pem_private_key(&self.private_key_pem)?;
        
        let signature = private_key
            .sign(Pkcs1v15Sign::new::<Sha256>(), digest)
            .map_err(|e| CryptoError::Signing(format!("RSA signing failed: {}", e)))?;

        Ok(signature)
    }
}

/// Proveedor que recupera los certificados de HashiCorp Vault dinámicamente
pub struct VaultCertificateProvider {
    client: VaultClient,
}

impl VaultCertificateProvider {
    pub fn new(client: VaultClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CertificateProvider for VaultCertificateProvider {
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError> {
        let secret = self.client.read_secret(key_ref).await?;
        let cert = secret
            .get("certificate")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CryptoError::Provider("Certificate not found in Vault secret".to_string()))?;
        
        Ok(clean_pem(cert))
    }

    async fn sign_sha256(&self, key_ref: &str, digest: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let secret = self.client.read_secret(key_ref).await?;
        let key_pem = secret
            .get("private_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CryptoError::Provider("Private key not found in Vault secret".to_string()))?;

        let private_key = parse_pem_private_key(key_pem)?;
        
        let signature = private_key
            .sign(Pkcs1v15Sign::new::<Sha256>(), digest)
            .map_err(|e| CryptoError::Signing(format!("RSA signing with Vault key failed: {}", e)))?;

        Ok(signature)
    }
}

/// Limpia las cabeceras PEM y saltos de línea para obtener el Base64 crudo del certificado
fn clean_pem(pem: &str) -> String {
    pem.replace("-----BEGIN CERTIFICATE-----", "")
       .replace("-----END CERTIFICATE-----", "")
       .replace("-----BEGIN PRIVATE KEY-----", "")
       .replace("-----END PRIVATE KEY-----", "")
       .replace("-----BEGIN RSA PRIVATE KEY-----", "")
       .replace("-----END RSA PRIVATE KEY-----", "")
       .replace('\n', "")
       .replace('\r', "")
       .trim()
       .to_string()
}

fn parse_pem_private_key(pem: &str) -> Result<RsaPrivateKey, CryptoError> {
    RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
        .map_err(|e| CryptoError::Provider(format!("Failed to parse private key PEM: {}", e)))
}
