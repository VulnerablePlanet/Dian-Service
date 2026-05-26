use thiserror::Error;

#[derive(Debug, Error)]
pub enum UblError {
    #[error("CUFE/CUDE calculation failed: {0}")]
    Calculation(String),

    #[error("XML payload validation failed: {0}")]
    Validation(String),

    #[error("XML template rendering failed: {0}")]
    Template(#[from] minijinja::Error),

    #[error("Formatting error: {0}")]
    Formatting(String),
}
