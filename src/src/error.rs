use thiserror::Error;
use solana_client::client_error::ClientError;
use solana_sdk::signature::SignerError;
use sqlx::Error as SqlxError;

#[derive(Error, Debug)]
pub enum VaultError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] SqlxError),
    
    #[error("Solana client error: {0}")]
    SolanaClientError(#[from] ClientError),
    
    #[error("Signer error: {0}")]
    SignerError(#[from] SignerError),
    
    #[error("Transaction failed: {0}")]
    TransactionFailed(String),
    
    #[error("Vault not found: {0}")]
    VaultNotFound(String),
    
    #[error("Insufficient balance: available={available}, required={required}")]
    InsufficientBalance { available: u64, required: u64 },
    
    #[error("Vault already exists: {0}")]
    VaultAlreadyExists(String),
    
    #[error("Invalid vault state: {0}")]
    InvalidVaultState(String),
    
    #[error("Unauthorized operation: {0}")]
    Unauthorized(String),
    
    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),
    
    #[error("Concurrent operation conflict: {0}")]
    ConcurrentConflict(String),
    
    #[error("Network error: {0}")]
    NetworkError(String),
    
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
    
    #[error("Validation error: {0}")]
    ValidationError(String),
    
    #[error("Timeout error: {0}")]
    TimeoutError(String),
    
    #[error("Internal error: {0}")]
    InternalError(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;