use crate::error::CryptoError;
use crate::vault::VaultClient;
use async_trait::async_trait;
use rsa::{pkcs8::DecodePrivateKey, RsaPrivateKey};
use rsa::Pkcs1v15Sign;
use sha2::Sha384;

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    /// Obtiene el certificado digital en formato PEM (codificado en Base64).
    async fn get_certificate(&self, key_ref: &str) -> Result<String, CryptoError>;

    /// Firma un hash pre-calculado (SHA-384) usando la llave privada.
    async fn sign_hash(&self, key_ref: &str, sha384_hash: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

/// Proveedor local para desarrollo/tests que almacena las llaves en memoria
pub struct LocalCertificateProvider {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[async_trait]
impl CertificateProvider for LocalCertificateProvider {
    async fn get_certificate(&self, _key_ref: &str) -> Result<String, CryptoError> {
        Ok(clean_pem(&self.certificate_pem))
    }

    async fn sign_hash(&self, _key_ref: &str, sha384_hash: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let private_key = parse_pem_private_key(&self.private_key_pem)?;
        
        let signature = private_key
            .sign(Pkcs1v15Sign::new::<Sha384>(), sha384_hash)
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

    async fn sign_hash(&self, key_ref: &str, sha384_hash: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let secret = self.client.read_secret(key_ref).await?;
        let key_pem = secret
            .get("private_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CryptoError::Provider("Private key not found in Vault secret".to_string()))?;

        let private_key = parse_pem_private_key(key_pem)?;
        
        let signature = private_key
            .sign(Pkcs1v15Sign::new::<Sha384>(), sha384_hash)
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
