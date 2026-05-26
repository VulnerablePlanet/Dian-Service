use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Domain logic error: {0}")]
    Domain(#[from] core_domain::ports::DomainError),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Circuit breaker is OPEN")]
    CircuitBreakerOpen,

    #[error("Internal worker failure: {0}")]
    Internal(String),
}
