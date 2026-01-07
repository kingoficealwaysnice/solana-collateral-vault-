use crate::error::{Result, VaultError};
use crate::models::{Vault, VaultCreateRequest, VaultDepositRequest, VaultWithdrawRequest, 
                    VaultLockRequest, VaultUnlockRequest, VaultTransferRequest, TransactionRecord,
                    TransactionType, TransactionStatus, BalanceSnapshot, AuditLog};
use crate::database::{VaultRepository, TransactionRepository, AuditRepository};
use sqlx::PgPool;
use chrono::{DateTime, Utc};
use uuid::Uuid;
use std::str::FromStr;
use solana_sdk::pubkey::Pubkey;
use tracing::{info, warn, error};

pub struct VaultManager {
    vault_repo: VaultRepository,
    transaction_repo: TransactionRepository,
    audit_repo: AuditRepository,
}

impl VaultManager {
    pub fn new(pool: PgPool) -> Self {
        Self {
            vault_repo: VaultRepository::new(pool.clone()),
            transaction_repo: TransactionRepository::new(pool.clone()),
            audit_repo: AuditRepository::new(pool),
        }
    }
    
    /// Initialize a new vault in the database
    pub async fn create_vault(&self, request: VaultCreateRequest, 
                              vault_pubkey: Pubkey, 
                              token_account_pubkey: Pubkey,
                              bump: u8) -> Result<Vault> {
        
        // Validate pubkeys
        let user_pubkey = Pubkey::from_str(&request.user_pubkey)
            .map_err(|_| VaultError::ValidationError("Invalid user pubkey".to_string()))?;
        let authority = Pubkey::from_str(&request.authority)
            .map_err(|_| VaultError::ValidationError("Invalid authority pubkey".to_string()))?;
        
        // Check if vault already exists
        match self.vault_repo.get_vault_by_user(&request.user_pubkey).await {
            Ok(_) => return Err(VaultError::VaultAlreadyExists(request.user_pubkey)),
            Err(VaultError::NotFound(_)) => {}, // This is expected - vault doesn't exist
            Err(e) => return Err(e), // Propagate other errors
        }
        
        // Create the vault
        let vault = self.vault_repo.create_vault(
            &request.user_pubkey,
            &vault_pubkey.to_string(),
            &token_account_pubkey.to_string()
        ).await?;
        
        // Log audit event
        self.audit_repo.log_event(
            "vault_created",
            Some(&request.user_pubkey),
            Some(vault.id),
            Some(serde_json::json!({
                "vault_pubkey": vault_pubkey.to_string(),
                "token_account": token_account_pubkey.to_string(),
                "bump": bump
            })),
            None
        ).await?;
        
        Ok(vault)
    }
    
    /// Get vault by user pubkey
    pub async fn get_vault_by_user(&self, user_pubkey: &str) -> Result<Option<Vault>> {
        match self.vault_repo.get_vault_by_user(user_pubkey).await {
            Ok(vault) => Ok(Some(vault)),
            Err(VaultError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
    
    /// Get vault by vault pubkey
    pub async fn get_vault_by_pubkey(&self, vault_pubkey: &str) -> Result<Option<Vault>> {
        // This would need to be implemented in VaultRepository
        // For now, we'll use a workaround
        let vaults = self.vault_repo.get_active_vaults(1000, 0).await?;
        Ok(vaults.into_iter().find(|v| v.vault_pubkey == vault_pubkey))
    }
    
    /// Get vault by ID
    pub async fn get_vault_by_id(&self, vault_id: Uuid) -> Result<Vault> {
        self.vault_repo.get_vault_by_id(vault_id).await
    }
    
    /// Update vault balances with audit logging
    pub async fn update_balances(&self, 
                                vault_id: Uuid,
                                new_total: i64,
                                new_locked: i64, 
                                new_available: i64,
                                tx_id: Option<Uuid>,
                                performed_by: &str) -> Result<Vault> {
        
        // Get current state for audit log
        let current = self.vault_repo.get_vault_by_id(vault_id).await?;
        
        // Update vault balances
        let updated_vault = self.vault_repo.update_vault_balances(
            vault_id,
            new_total,
            new_locked,
            new_available
        ).await?;
        
        // Log audit event
        self.audit_repo.log_event(
            "balance_updated",
            Some(&current.user_pubkey),
            Some(vault_id),
            Some(serde_json::json!({
                "old_total": current.total_balance,
                "new_total": new_total,
                "old_locked": current.locked_balance,
                "new_locked": new_locked,
                "old_available": current.available_balance,
                "new_available": new_available,
                "transaction_id": tx_id
            })),
            Some(serde_json::json!({"performed_by": performed_by}))
        ).await?;
        
        Ok(updated_vault)
    }
    
    /// Deactivate vault (emergency shutdown)
    pub async fn deactivate_vault(&self, vault_id: Uuid, reason: &str) -> Result<Vault> {
        let vault = self.vault_repo.deactivate_vault(vault_id).await?;
        
        // Log emergency action
        self.audit_repo.log_event(
            "vault_deactivated",
            Some(&vault.user_pubkey),
            Some(vault_id),
            Some(serde_json::json!({"reason": reason})),
            None
        ).await?;
        
        Ok(vault)
    }
    
    /// Get all active vaults (for monitoring)
    pub async fn get_active_vaults(&self, limit: i64, offset: i64) -> Result<Vec<Vault>> {
        self.vault_repo.get_active_vaults(limit as i32, offset as i32).await
    }
    
    /// Get total value locked across all vaults
    pub async fn get_total_value_locked(&self) -> Result<i64> {
        let stats = self.vault_repo.get_active_vaults(10000, 0).await?;
        Ok(stats.iter().map(|v| v.total_balance).sum())
    }
    
    /// Create balance snapshot for reconciliation
    pub async fn create_balance_snapshot(&self, vault_id: Uuid, block_height: Option<i64>) -> Result<BalanceSnapshot> {
        let vault = self.get_vault_by_id(vault_id).await?;
        
        // This would need to be implemented in SnapshotRepository
        // For now, we'll create it directly
        let snapshot = sqlx::query_as!(
            BalanceSnapshot,
            r#"
            INSERT INTO balance_snapshots (vault_id, total_balance, locked_balance, 
                                         available_balance, block_height, snapshot_time)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, vault_id, total_balance, locked_balance, available_balance, snapshot_time, block_height
            "#,
            vault_id,
            vault.total_balance,
            vault.locked_balance,
            vault.available_balance,
            block_height,
            Utc::now(),
        )
        .fetch_one(&self.vault_repo.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to create balance snapshot: {}", e)))?;
        
        Ok(snapshot)
    }
}

/// Transaction manager for tracking on-chain operations
pub struct TransactionManager {
    transaction_repo: TransactionRepository,
    audit_repo: AuditRepository,
}

impl TransactionManager {
    pub fn new(pool: PgPool) -> Self {
        Self {
            transaction_repo: TransactionRepository::new(pool.clone()),
            audit_repo: AuditRepository::new(pool),
        }
    }
    
    /// Create transaction record
    pub async fn create_transaction(&self, 
                                  vault_id: Uuid,
                                  tx_type: TransactionType,
                                  amount: i64,
                                  tx_signature: Option<String>,
                                  idempotency_key: Option<String>) -> Result<TransactionRecord> {
        
        let tx = self.transaction_repo.create_transaction(
            vault_id,
            &format!("{:?}", tx_type).to_lowercase(),
            amount,
            tx_signature.as_deref(),
            idempotency_key.as_deref()
        ).await?;
        
        // Log audit event
        self.audit_repo.log_event(
            "transaction_created",
            None, // user_pubkey would need to be fetched from vault
            Some(vault_id),
            Some(serde_json::json!({
                "transaction_type": format!("{:?}", tx_type),
                "amount": amount,
                "signature": tx_signature
            })),
            None
        ).await?;
        
        Ok(tx)
    }
    
    /// Update transaction status
    pub async fn update_transaction_status(&self, 
                                         tx_id: Uuid,
                                         status: TransactionStatus,
                                         error_message: Option<String>) -> Result<TransactionRecord> {
        
        let tx = self.transaction_repo.update_transaction_status(
            tx_id,
            &format!("{:?}", status).to_lowercase(),
            None, // signature won't change
            error_message.as_deref()
        ).await?;
        
        // Log audit event for status changes
        if matches!(status, TransactionStatus::Failed | TransactionStatus::Confirmed) {
            self.audit_repo.log_event(
                "transaction_status_updated",
                None,
                Some(tx.vault_id),
                Some(serde_json::json!({
                    "transaction_id": tx_id,
                    "new_status": format!("{:?}", status),
                    "error": error_message
                })),
                None
            ).await?;
        }
        
        Ok(tx)
    }
    
    /// Get pending transactions
    pub async fn get_pending_transactions(&self, limit: i64) -> Result<Vec<TransactionRecord>> {
        // This would need to be implemented in TransactionRepository
        let transactions = sqlx::query_as!(
            TransactionRecord,
            r#"
            SELECT id, vault_id, operation_type, amount, signature, status, error_message, idempotency_key, created_at, updated_at
            FROM transaction_records 
            WHERE status = 'pending' 
            ORDER BY created_at ASC 
            LIMIT $1
            "#,
            limit
        )
        .fetch_all(&self.transaction_repo.pool)
        .await
        .map_err(|e| VaultError::DatabaseError(format!("Failed to get pending transactions: {}", e)))?;
        
        Ok(transactions)
    }
    
    /// Get transactions by vault
    pub async fn get_vault_transactions(&self, vault_id: Uuid, limit: i64) -> Result<Vec<TransactionRecord>> {
        self.transaction_repo.get_vault_transactions(vault_id, limit as i32).await
    }
    
    /// Get transaction by idempotency key
    pub async fn get_transaction_by_idempotency_key(&self, idempotency_key: &str) -> Result<Option<TransactionRecord>> {
        self.transaction_repo.get_transaction_by_idempotency_key(idempotency_key).await
    }
}