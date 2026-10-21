use crate::error::CryptoError;
use crate::provider::{CertificateProvider, LocalCertificateProvider, VaultCertificateProvider};
use crate::sign_document;
use crate::vault::VaultClient;
use chrono::Utc;
use core_domain::ports::DocumentRepository;
use std::sync::Arc;
use uuid::Uuid;

/// Cómo se elige el certificado con el que se firma cada documento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMode {
    /// Certificado activo del tenant (`certificates.vault_reference_path`) leído de Vault.
    PerTenantVault,
    /// Un único par local para todos los tenants. Pensado para desarrollo y pruebas.
    SharedLocal,
}

/// Firmador configurado al arrancar. No existe un modo "por defecto": si no hay
/// proveedor configurado, [`SignerConfig::from_env`] devuelve error y el proceso no arranca.
pub struct SignerConfig {
    provider: Arc<dyn CertificateProvider>,
    mode: KeyMode,
}

impl SignerConfig {
    pub fn new(provider: Arc<dyn CertificateProvider>, mode: KeyMode) -> Self {
        Self { provider, mode }
    }

    /// Lee la configuración del entorno:
    /// - `VAULT_ADDR` + `VAULT_TOKEN`: certificado por tenant desde Vault.
    /// - `SIGNER_CERT_PATH` + `SIGNER_KEY_PATH`: par local compartido (solo desarrollo).
    pub fn from_env() -> Result<Self, CryptoError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());

        if let (Some(addr), Some(token)) = (var("VAULT_ADDR"), var("VAULT_TOKEN")) {
            let client = VaultClient::new(addr, token)?;
            return Ok(Self::new(
                Arc::new(VaultCertificateProvider::new(client)),
                KeyMode::PerTenantVault,
            ));
        }

        if let (Some(cert_path), Some(key_path)) = (var("SIGNER_CERT_PATH"), var("SIGNER_KEY_PATH")) {
            let read = |path: &str| {
                std::fs::read_to_string(path)
                    .map_err(|e| CryptoError::Provider(format!("Cannot read {}: {}", path, e)))
            };
            let provider = LocalCertificateProvider::from_pem(read(&cert_path)?, read(&key_path)?)?;
            return Ok(Self::new(Arc::new(provider), KeyMode::SharedLocal));
        }

        Err(CryptoError::Provider(
            "No signing provider configured: set VAULT_ADDR and VAULT_TOKEN, \
             or SIGNER_CERT_PATH and SIGNER_KEY_PATH"
                .to_string(),
        ))
    }

    pub fn mode(&self) -> KeyMode {
        self.mode
    }

    /// Firma `xml` con el certificado que corresponde al tenant.
    pub async fn sign_for_tenant(
        &self,
        repo: &dyn DocumentRepository,
        tenant_id: Uuid,
        xml: &str,
    ) -> Result<String, CryptoError> {
        let key_ref = match self.mode {
            KeyMode::SharedLocal => "local".to_string(),
            KeyMode::PerTenantVault => {
                let cert = repo
                    .find_active_certificate_by_tenant(tenant_id)
                    .await
                    .map_err(|e| CryptoError::Provider(format!("Certificate lookup failed: {}", e)))?
                    .ok_or_else(|| {
                        CryptoError::Provider(format!("Tenant {} has no active certificate", tenant_id))
                    })?;
                if cert.expiration_date <= Utc::now() {
                    return Err(CryptoError::Provider(format!(
                        "Active certificate {} of tenant {} expired on {}",
                        cert.id, tenant_id, cert.expiration_date
                    )));
                }
                cert.vault_reference_path
            }
        };
        sign_document(xml, self.provider.as_ref(), &key_ref).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use core_domain::models::{Certificate, CertificateStatus};
    use core_domain::testing::InMemoryRepository;

    const TEST_KEY_PEM: &str = include_str!("../tests/fixtures/test_key.pem");
    const TEST_CERT_PEM: &str = include_str!("../tests/fixtures/test_cert.pem");
    const XML: &str = r#"<Invoice xmlns="urn:test"><ID>1</ID><!-- PLACEHOLDER_FOR_SIGNATURE --></Invoice>"#;

    fn per_tenant_config() -> SignerConfig {
        let provider = LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), TEST_KEY_PEM.into()).unwrap();
        SignerConfig::new(Arc::new(provider), KeyMode::PerTenantVault)
    }

    fn certificate(tenant_id: Uuid, expires_in: Duration) -> Certificate {
        Certificate {
            id: Uuid::new_v4(),
            tenant_id,
            vault_reference_path: "tenants/acme/cert".into(),
            thumbprint: "thumb".into(),
            expiration_date: Utc::now() + expires_in,
            status: CertificateStatus::Active,
        }
    }

    #[test]
    fn local_provider_rejects_invalid_pem_at_construction() {
        assert!(LocalCertificateProvider::from_pem("cert_dummy".into(), "key_dummy".into()).is_err());
        assert!(LocalCertificateProvider::from_pem(TEST_CERT_PEM.into(), "key_dummy".into()).is_err());
    }

    #[tokio::test]
    async fn per_tenant_mode_requires_an_active_certificate() {
        let repo = InMemoryRepository::default();
        let err = per_tenant_config()
            .sign_for_tenant(&repo, Uuid::new_v4(), XML)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no active certificate"));
    }

    #[tokio::test]
    async fn per_tenant_mode_rejects_expired_certificate() {
        let tenant_id = Uuid::new_v4();
        let repo = InMemoryRepository::default();
        repo.save_certificate(&certificate(tenant_id, Duration::days(-1))).await.unwrap();
        let err = per_tenant_config()
            .sign_for_tenant(&repo, tenant_id, XML)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[tokio::test]
    async fn per_tenant_mode_signs_with_valid_certificate() {
        let tenant_id = Uuid::new_v4();
        let repo = InMemoryRepository::default();
        repo.save_certificate(&certificate(tenant_id, Duration::days(30))).await.unwrap();
        let signed = per_tenant_config().sign_for_tenant(&repo, tenant_id, XML).await.unwrap();
        assert!(signed.contains("<ds:SignatureValue>"));
    }
}
