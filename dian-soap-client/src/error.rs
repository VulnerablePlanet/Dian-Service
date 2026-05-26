use thiserror::Error;

#[derive(Debug, Error)]
pub enum SoapError {
    #[error("ZIP Compression failed: {0}")]
    Zip(String),

    #[error("Network HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("XML parsing error: {0}")]
    Xml(String),

    #[error("SOAP Fault returned by DIAN: [{code}] {message}")]
    Fault { code: String, message: String },

    #[error("DIAN processing error: {0}")]
    Dian(String),
}

impl From<SoapError> for core_domain::ports::DomainError {
    fn from(err: SoapError) -> Self {
        match err {
            SoapError::Zip(msg) => core_domain::ports::DomainError::Internal(format!("Zip: {}", msg)),
            SoapError::Http(e) => core_domain::ports::DomainError::Soap(format!("Http network failure: {}", e)),
            SoapError::Xml(msg) => core_domain::ports::DomainError::Soap(format!("Xml parsing: {}", msg)),
            SoapError::Fault { code, message } => {
                core_domain::ports::DomainError::Soap(format!("Soap fault [{}]: {}", code, message))
            }
            SoapError::Dian(msg) => core_domain::ports::DomainError::Soap(format!("Dian error: {}", msg)),
        }
    }
}
