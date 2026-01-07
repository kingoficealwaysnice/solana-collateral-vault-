use crate::error::{Result, VaultError};
use crate::models::{Vault, TransactionRecord, BalanceSnapshot, SystemBalanceStats};
use sqlx::{PgPool, Row};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use std::collections::HashMap;
use tracing::{info, warn, error};

/// Database operations for vault management
pub struct VaultRepository {
    pool: PgPool,
}

impl VaultRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new vault
    pub async fn create_vault(&self, user_pubkey: &str, vault_pubkey: &str, token_account: &str) -> Result<Vault> {
        let vault = sqlx::query_as!(
            Vault,
            r#"
            INSERT INTO vaults (user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at)
            VALUES ($1, $2, $3, 0, 0, 0, true, NOW(), NOW())
            RETURNING id, user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at
            "#,
            user_pubkey,
            vault_pubkey,
            token_account
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to create vault: {}", e)))?;

        info!("Created vault {} for user {}", vault.id, user_pubkey);
        Ok(vault)
    }

    /// Get vault by ID
    pub async fn get_vault_by_id(&self, vault_id: Uuid) -> Result<Vault> {
        let vault = sqlx::query_as!(
            Vault,
            r#"
            SELECT id, user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at
            FROM vaults
            WHERE id = $1
            "#,
            vault_id
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::NotFound(format!("Vault {} not found: {}", vault_id, e)))?;

        Ok(vault)
    }

    /// Get vault by user pubkey
    pub async fn get_vault_by_user(&self, user_pubkey: &str) -> Result<Vault> {
        let vault = sqlx::query_as!(
            Vault,
            r#"
            SELECT id, user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at
            FROM vaults
            WHERE user_pubkey = $1 AND is_active = true
            "#,
            user_pubkey
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::NotFound(format!("Vault for user {} not found: {}", user_pubkey, e)))?;

        Ok(vault)
    }

    /// Update vault balances
    pub async fn update_vault_balances(&self, vault_id: Uuid, total: i64, locked: i64, available: i64) -> Result<Vault> {
        // Validate balance invariant
        if total != locked + available {
            return Err(VaultError::InvalidInput(format!(
                "Balance invariant violation: total={} != locked={} + available={}",
                total, locked, available
            )));
        }

        let vault = sqlx::query_as!(
            Vault,
            r#"
            UPDATE vaults 
            SET total_balance = $2, locked_balance = $3, available_balance = $4, updated_at = NOW()
            WHERE id = $1
            RETURNING id, user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at
            "#,
            vault_id,
            total,
            locked,
            available
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to update vault balances: {}", e)))?;

        info!("Updated balances for vault {}: total={}, locked={}, available={}", 
              vault_id, total, locked, available);
        Ok(vault)
    }

    /// List active vaults with pagination
    pub async fn get_active_vaults(&self, limit: i32, offset: i32) -> Result<Vec<Vault>> {
        let vaults = sqlx::query_as!(
            Vault,
            r#"
            SELECT id, user_pubkey, vault_pubkey, token_account_pubkey, total_balance, locked_balance, available_balance, is_active, created_at, updated_at
            FROM vaults
            WHERE is_active = true
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "#,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to list vaults: {}", e)))?;

        Ok(vaults)
    }

    /// Deactivate vault
    pub async fn deactivate_vault(&self, vault_id: Uuid) -> Result<()> {
        let result = sqlx::query!(
            r#"
            UPDATE vaults 
            SET is_active = false, updated_at = NOW()
            WHERE id = $1
            "#,
            vault_id
        )
        .execute(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to deactivate vault: {}", e)))?;

        if result.rows_affected() == 0 {
            return Err(VaultError::NotFound(format!("Vault {} not found", vault_id)));
        }

        info!("Deactivated vault {}", vault_id);
        Ok(())
    }
}

/// Database operations for transaction management
pub struct TransactionRepository {
    pool: PgPool,
}

impl TransactionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create transaction record
    pub async fn create_transaction(
        &self,
        vault_id: Uuid,
        operation_type: &str,
        amount: i64,
        signature: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<TransactionRecord> {
        let tx = sqlx::query_as!(
            TransactionRecord,
            r#"
            INSERT INTO transaction_records (vault_id, operation_type, amount, signature, status, idempotency_key, created_at, updated_at)
            VALUES ($1, $2, $3, $4, 'pending', $5, NOW(), NOW())
            RETURNING id, vault_id, operation_type, amount, signature, status, error_message, idempotency_key, created_at, updated_at
            "#,
            vault_id,
            operation_type,
            amount,
            signature,
            idempotency_key
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to create transaction record: {}", e)))?;

        info!("Created transaction {} for vault {}: {} {}", tx.id, vault_id, operation_type, amount);
        Ok(tx)
    }

    /// Update transaction status
    pub async fn update_transaction_status(
        &self,
        transaction_id: Uuid,
        status: &str,
        signature: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<TransactionRecord> {
        let tx = sqlx::query_as!(
            TransactionRecord,
            r#"
            UPDATE transaction_records 
            SET status = $2, signature = COALESCE($3, signature), error_message = $4, updated_at = NOW()
            WHERE id = $1
            RETURNING id, vault_id, operation_type, amount, signature, status, error_message, idempotency_key, created_at, updated_at
            "#,
            transaction_id,
            status,
            signature,
            error_message
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to update transaction status: {}", e)))?;

        info!("Updated transaction {} status to {}", transaction_id, status);
        Ok(tx)
    }

    /// Get transaction by idempotency key
    pub async fn get_transaction_by_idempotency_key(&self, idempotency_key: &str) -> Result<Option<TransactionRecord>> {
        let tx = sqlx::query_as!(
            TransactionRecord,
            r#"
            SELECT id, vault_id, operation_type, amount, signature, status, error_message, idempotency_key, created_at, updated_at
            FROM transaction_records
            WHERE idempotency_key = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            idempotency_key
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get transaction by idempotency key: {}", e)))?;

        Ok(tx)
    }

    /// Get recent transactions for vault
    pub async fn get_vault_transactions(&self, vault_id: Uuid, limit: i32) -> Result<Vec<TransactionRecord>> {
        let transactions = sqlx::query_as!(
            TransactionRecord,
            r#"
            SELECT id, vault_id, operation_type, amount, signature, status, error_message, idempotency_key, created_at, updated_at
            FROM transaction_records
            WHERE vault_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
            vault_id,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get vault transactions: {}", e)))?;

        Ok(transactions)
    }

    /// Get pending transactions count
    pub async fn get_pending_transactions_count(&self) -> Result<i64> {
        let count = sqlx::query!(
            r#"
            SELECT COUNT(*) as count FROM transaction_records WHERE status = 'pending'
            "#
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count.count.unwrap_or(0))
    }

    /// Cleanup stale pending transactions
    pub async fn cleanup_stale_transactions(&self, cutoff_time: DateTime<Utc>) -> Result<i64> {
        let result = sqlx::query!(
            r#"
            UPDATE transaction_records 
            SET status = 'failed', error_message = 'Transaction expired', updated_at = NOW()
            WHERE status = 'pending' AND created_at < $1
            "#,
            cutoff_time
        )
        .execute(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to cleanup stale transactions: {}", e)))?;

        Ok(result.rows_affected() as i64)
    }
}

/// Database operations for balance snapshots
pub struct SnapshotRepository {
    pool: PgPool,
}

impl SnapshotRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create balance snapshot
    pub async fn create_snapshot(
        &self,
        vault_id: Uuid,
        total_balance: i64,
        locked_balance: i64,
        available_balance: i64,
        block_height: Option<i64>,
    ) -> Result<BalanceSnapshot> {
        let snapshot = sqlx::query_as!(
            BalanceSnapshot,
            r#"
            INSERT INTO balance_snapshots (vault_id, total_balance, locked_balance, available_balance, block_height, created_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            RETURNING id, vault_id, total_balance, locked_balance, available_balance, block_height, created_at
            "#,
            vault_id,
            total_balance,
            locked_balance,
            available_balance,
            block_height
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to create balance snapshot: {}", e)))?;

        Ok(snapshot)
    }

    /// Get recent snapshots for vault
    pub async fn get_vault_snapshots(&self, vault_id: Uuid, limit: i32) -> Result<Vec<BalanceSnapshot>> {
        let snapshots = sqlx::query_as!(
            BalanceSnapshot,
            r#"
            SELECT id, vault_id, total_balance, locked_balance, available_balance, block_height, created_at
            FROM balance_snapshots
            WHERE vault_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
            vault_id,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get vault snapshots: {}", e)))?;

        Ok(snapshots)
    }

    /// Get system-wide balance statistics
    pub async fn get_system_stats(&self) -> Result<SystemBalanceStats> {
        let stats = sqlx::query!(
            r#"
            SELECT 
                COALESCE(SUM(total_balance), 0) as total_value_locked,
                COALESCE(SUM(locked_balance), 0) as total_locked,
                COALESCE(SUM(available_balance), 0) as total_available,
                COUNT(*) as vault_count
            FROM vaults
            WHERE is_active = true
            "#
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get system stats: {}", e)))?;

        Ok(SystemBalanceStats {
            total_value_locked: stats.total_value_locked.unwrap_or(0),
            total_locked: stats.total_locked.unwrap_or(0),
            total_available: stats.total_available.unwrap_or(0),
            vault_count: stats.vault_count.unwrap_or(0),
        })
    }
}

/// Database operations for audit logs
pub struct AuditRepository {
    pool: PgPool,
}

impl AuditRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Log audit event
    pub async fn log_event(
        &self,
        event_type: &str,
        user_pubkey: Option<&str>,
        vault_id: Option<Uuid>,
        details: Option<serde_json::Value>,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO audit_logs (event_type, user_pubkey, vault_id, details, metadata, created_at)
            VALUES ($1, $2, $3, $4, $5, NOW())
            "#,
            event_type,
            user_pubkey,
            vault_id,
            details,
            metadata
        )
        .execute(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to log audit event: {}", e)))?;

        Ok(())
    }

    /// Get recent audit events
    pub async fn get_recent_events(&self, limit: i32) -> Result<Vec<crate::models::AuditLog>> {
        let events = sqlx::query_as!(
            crate::models::AuditLog,
            r#"
            SELECT id, event_type, user_pubkey, vault_id, details, metadata, created_at
            FROM audit_logs
            ORDER BY created_at DESC
            LIMIT $1
            "#,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get audit events: {}", e)))?;

        Ok(events)
    }

    /// Get audit events for vault
    pub async fn get_vault_events(&self, vault_id: Uuid, limit: i32) -> Result<Vec<crate::models::AuditLog>> {
        let events = sqlx::query_as!(
            crate::models::AuditLog,
            r#"
            SELECT id, event_type, user_pubkey, vault_id, details, metadata, created_at
            FROM audit_logs
            WHERE vault_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
            vault_id,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get vault audit events: {}", e)))?;

        Ok(events)
    }
}

/// Rate limiting operations
pub struct RateLimitRepository {
    pool: PgPool,
}

impl RateLimitRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Consume rate limit tokens
    pub async fn consume_tokens(
        &self,
        bucket_key: &str,
        tokens_to_consume: i32,
        max_tokens: i32,
        refill_rate: i32,
    ) -> Result<crate::models::RateLimitResult> {
        let result = sqlx::query!(
            r#"
            SELECT * FROM consume_rate_limit_token($1, $2, $3, $4)
            "#,
            bucket_key,
            tokens_to_consume,
            max_tokens,
            refill_rate
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to consume rate limit tokens: {}", e)))?;

        Ok(crate::models::RateLimitResult {
            allowed: result.allowed.unwrap_or(false),
            remaining_tokens: result.remaining_tokens.unwrap_or(0),
            reset_at: result.reset_at,
        })
    }
}