use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Canonicalization failed: {0}")]
    C14n(String),

    #[error("Vault error: {0}")]
    Vault(String),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Signing failed: {0}")]
    Signing(String),

    #[error("XML parsing/formatting error: {0}")]
    Xml(String),

    #[error("Serialization/Deserialization failed: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<CryptoError> for core_domain::ports::DomainError {
    fn from(err: CryptoError) -> Self {
        match err {
            CryptoError::C14n(msg) => core_domain::ports::DomainError::Crypto(format!("C14n: {}", msg)),
            CryptoError::Vault(msg) => core_domain::ports::DomainError::Soap(format!("Vault: {}", msg)), // Or other suitable
            CryptoError::Provider(msg) => core_domain::ports::DomainError::Crypto(format!("Provider: {}", msg)),
            CryptoError::Signing(msg) => core_domain::ports::DomainError::Crypto(format!("Signing: {}", msg)),
            CryptoError::Xml(msg) => core_domain::ports::DomainError::Crypto(format!("Xml: {}", msg)),
            CryptoError::Serde(e) => core_domain::ports::DomainError::Internal(format!("Serde: {}", e)),
        }
    }
}
