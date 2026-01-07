use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Vault {
    pub id: Uuid,
    pub user_pubkey: String,
    pub vault_pubkey: String,
    pub token_account_pubkey: String,
    pub bump: i32,
    pub total_balance: i64,      // Stored as micro-USDT to avoid floating point
    pub locked_balance: i64,
    pub available_balance: i64,
    pub last_updated: DateTime<Utc>,
    pub is_active: bool,
    pub authority: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultCreateRequest {
    pub user_pubkey: String,
    pub authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDepositRequest {
    pub user_pubkey: String,
    pub amount: u64, // In USDT with 6 decimals
    pub user_token_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWithdrawRequest {
    pub user_pubkey: String,
    pub amount: u64, // In USDT with 6 decimals
    pub user_token_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultLockRequest {
    pub user_pubkey: String,
    pub amount: u64,
    pub caller_authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultUnlockRequest {
    pub user_pubkey: String,
    pub amount: u64,
    pub caller_authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultTransferRequest {
    pub source_user_pubkey: String,
    pub destination_user_pubkey: String,
    pub amount: u64,
    pub caller_authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultResponse {
    pub id: Uuid,
    pub user_pubkey: String,
    pub vault_pubkey: String,
    pub token_account_pubkey: String,
    pub total_balance: u64,
    pub locked_balance: u64,
    pub available_balance: u64,
    pub last_updated: DateTime<Utc>,
    pub is_active: bool,
    pub authority: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub id: Uuid,
    pub vault_id: Uuid,
    pub transaction_type: TransactionType,
    pub amount: i64,
    pub tx_signature: Option<String>,
    pub status: TransactionStatus,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "transaction_type", rename_all = "snake_case")]
pub enum TransactionType {
    Initialize,
    Deposit,
    Withdraw,
    Lock,
    Unlock,
    Transfer,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "transaction_status", rename_all = "snake_case")]
pub enum TransactionStatus {
    Pending,
    Processing,
    Confirmed,
    Failed,
    Reverted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceSnapshot {
    pub id: Uuid,
    pub vault_id: Uuid,
    pub total_balance: i64,
    pub locked_balance: i64,
    pub available_balance: i64,
    pub snapshot_time: DateTime<Utc>,
    pub block_height: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: Uuid,
    pub event_type: String,
    pub user_pubkey: Option<String>,
    pub vault_id: Option<Uuid>,
    pub details: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitResult {
    pub allowed: bool,
    pub remaining_tokens: i32,
    pub reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBalanceStats {
    pub total_value_locked: i64,
    pub total_locked: i64,
    pub total_available: i64,
    pub vault_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub status: String,
    pub timestamp: DateTime<Utc>,
    pub solana_connection: bool,
    pub database_connection: bool,
    pub vault_count: i64,
    pub pending_transactions: i64,
    pub total_value_locked: i64,
}

// Conversion helpers
impl Vault {
    pub fn to_response(&self) -> VaultResponse {
        VaultResponse {
            id: self.id,
            user_pubkey: self.user_pubkey.clone(),
            vault_pubkey: self.vault_pubkey.clone(),
            token_account_pubkey: self.token_account_pubkey.clone(),
            total_balance: self.total_balance as u64,
            locked_balance: self.locked_balance as u64,
            available_balance: self.available_balance as u64,
            last_updated: self.last_updated,
            is_active: self.is_active,
            authority: self.authority.clone(),
        }
    }
    
    pub fn validate_pubkey(&self) -> Result<Pubkey, String> {
        Pubkey::from_str(&self.user_pubkey).map_err(|e| e.to_string())
    }
}

impl VaultResponse {
    pub fn validate_balances(&self) -> bool {
        self.total_balance == (self.locked_balance + self.available_balance)
    }
}