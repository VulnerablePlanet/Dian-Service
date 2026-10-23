use crate::error::CryptoError;
use crate::provider::{CertificateProvider, LocalCertificateProvider, VaultKvKeyProvider, VaultTransitProvider};
use crate::sign_document;
use crate::wssec::sign_soap_envelope;
use crate::vault::VaultClient;
use chrono::Utc;
use async_trait::async_trait;
use core_domain::ports::{CertificateRepository, CryptoSigner, DomainError};
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
    /// - `VAULT_ADDR` + `VAULT_TOKEN`: certificado por tenant desde Vault. Con
    ///   `VAULT_SIGNING_MODE=transit` (por defecto) la llave vive en Vault Transit y nunca
    ///   sale de él; `kv` (legado) descarga la llave privada de KV v2. Mounts:
    ///   `VAULT_KV_MOUNT` (`secret`) y `VAULT_TRANSIT_MOUNT` (`transit`).
    /// - `SIGNER_CERT_PATH` + `SIGNER_KEY_PATH`: par local compartido (solo desarrollo).
    pub fn from_env() -> Result<Self, CryptoError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());

        if let (Some(addr), Some(token)) = (var("VAULT_ADDR"), var("VAULT_TOKEN")) {
            let client = VaultClient::new(addr, token)?.with_mounts(
                var("VAULT_KV_MOUNT").unwrap_or_else(|| "secret".to_string()),
                var("VAULT_TRANSIT_MOUNT").unwrap_or_else(|| "transit".to_string()),
            )?;
            let provider: Arc<dyn CertificateProvider> = match var("VAULT_SIGNING_MODE").as_deref() {
                None | Some("transit") => Arc::new(VaultTransitProvider::new(client)),
                Some("kv") => Arc::new(VaultKvKeyProvider::new(client)),
                Some(other) => {
                    return Err(CryptoError::Provider(format!(
                        "Invalid VAULT_SIGNING_MODE '{}': use 'transit' or 'kv'",
                        other
                    )))
                }
            };
            return Ok(Self::new(provider, KeyMode::PerTenantVault));
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
        certificates: &dyn CertificateRepository,
        tenant_id: Uuid,
        xml: &str,
    ) -> Result<String, CryptoError> {
        let key_ref = self.key_ref(certificates, tenant_id).await?;
        sign_document(xml, self.provider.as_ref(), &key_ref).await
    }

    /// Firma la cabecera WS-Security del envelope con el certificado del tenant.
    pub async fn sign_soap_for_tenant(
        &self,
        certificates: &dyn CertificateRepository,
        tenant_id: Uuid,
        envelope: &str,
    ) -> Result<String, CryptoError> {
        let key_ref = self.key_ref(certificates, tenant_id).await?;
        sign_soap_envelope(envelope, self.provider.as_ref(), &key_ref).await
    }

    /// Referencia de la llave del tenant: su certificado activo y vigente, o el par local.
    async fn key_ref(&self, certificates: &dyn CertificateRepository, tenant_id: Uuid) -> Result<String, CryptoError> {
        Ok(match self.mode {
            KeyMode::SharedLocal => "local".to_string(),
            KeyMode::PerTenantVault => {
                let cert = certificates
                    .find_active_certificate(tenant_id)
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
        })
    }
}

/// Implementación del puerto `CryptoSigner`: firma con el certificado del tenant.
pub struct TenantSigner {
    config: SignerConfig,
    certificates: Arc<dyn CertificateRepository>,
}

impl TenantSigner {
    pub fn new(config: SignerConfig, certificates: Arc<dyn CertificateRepository>) -> Self {
        Self { config, certificates }
    }

    pub fn mode(&self) -> KeyMode {
        self.config.mode()
    }
}

#[async_trait]
impl CryptoSigner for TenantSigner {
    async fn sign_for_tenant(&self, tenant_id: Uuid, xml: &str) -> Result<String, DomainError> {
        self.config
            .sign_for_tenant(self.certificates.as_ref(), tenant_id, xml)
            .await
            .map_err(DomainError::from)
    }

    async fn sign_soap_for_tenant(&self, tenant_id: Uuid, envelope: &str) -> Result<String, DomainError> {
        self.config
            .sign_soap_for_tenant(self.certificates.as_ref(), tenant_id, envelope)
            .await
            .map_err(DomainError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use core_domain::models::{Certificate, CertificateStatus};
    use core_domain::testing::InMemoryStore;

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
        let repo = InMemoryStore::default();
        let err = per_tenant_config()
            .sign_for_tenant(&repo, Uuid::new_v4(), XML)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no active certificate"));
    }

    #[tokio::test]
    async fn per_tenant_mode_rejects_expired_certificate() {
        let tenant_id = Uuid::new_v4();
        let repo = InMemoryStore::default();
        repo.add_certificate(certificate(tenant_id, Duration::days(-1)));
        let err = per_tenant_config()
            .sign_for_tenant(&repo, tenant_id, XML)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[tokio::test]
    async fn tenant_signer_implements_the_port() {
        let tenant_id = Uuid::new_v4();
        let repo = Arc::new(InMemoryStore::default());
        repo.add_certificate(certificate(tenant_id, Duration::days(30)));
        let signer: Arc<dyn CryptoSigner> = Arc::new(TenantSigner::new(per_tenant_config(), repo));
        assert!(signer.sign_for_tenant(tenant_id, XML).await.unwrap().contains("<ds:SignatureValue"));
        assert!(matches!(
            signer.sign_for_tenant(Uuid::new_v4(), XML).await,
            Err(DomainError::Crypto(_))
        ));
    }

    #[tokio::test]
    async fn per_tenant_mode_signs_with_valid_certificate() {
        let tenant_id = Uuid::new_v4();
        let repo = InMemoryStore::default();
        repo.add_certificate(certificate(tenant_id, Duration::days(30)));
        let signed = per_tenant_config().sign_for_tenant(&repo, tenant_id, XML).await.unwrap();
        assert!(signed.contains("<ds:SignatureValue"));
    }

    #[tokio::test]
    async fn soap_envelopes_are_signed_with_the_tenant_certificate() {
        let tenant_id = Uuid::new_v4();
        let repo = Arc::new(InMemoryStore::default());
        repo.add_certificate(certificate(tenant_id, Duration::days(30)));
        let signer: Arc<dyn CryptoSigner> = Arc::new(TenantSigner::new(per_tenant_config(), repo));
        let envelope = dian_soap_envelope();

        let signed = signer.sign_soap_for_tenant(tenant_id, &envelope).await.unwrap();
        assert!(signed.contains("<wsse:BinarySecurityToken") && signed.contains("<ds:SignatureValue>"));
        assert!(
            matches!(signer.sign_soap_for_tenant(Uuid::new_v4(), &envelope).await, Err(DomainError::Crypto(_))),
            "a tenant without an active certificate cannot talk to the DIAN"
        );
    }

    fn dian_soap_envelope() -> String {
        format!(
            concat!(
                r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://www.w3.org/2005/08/addressing" "#,
                r#"xmlns:wsse="{}" xmlns:wsu="{}"><soap:Header><wsse:Security>{}</wsse:Security>"#,
                r#"<wsa:To wsu:Id="id-To">https://example.test</wsa:To></soap:Header><soap:Body/></soap:Envelope>"#
            ),
            crate::wssec::NS_WSSE,
            crate::wssec::NS_WSU,
            core_domain::ports::WSSE_SECURITY_PLACEHOLDER
        )
    }
}
