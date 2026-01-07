use crate::error::{Result, VaultError};
use crate::models::{Vault, TransactionRecord, TransactionType, TransactionStatus};
use crate::vault_manager::VaultManager;
use crate::transaction_builder::{TransactionBuilder, TransactionSubmitter, BuiltTransaction};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
use std::collections::HashMap;
use chrono::{DateTime, Utc, Duration};
use tracing::{info, warn, error};

/// CPI Manager handles cross-program invocations for trading operations
pub struct CPIManager {
    vault_manager: Arc<VaultManager>,
    transaction_builder: Arc<TransactionBuilder>,
    transaction_submitter: Arc<TransactionSubmitter>,
    authority_keypair: Arc<Keypair>,
    pending_operations: Arc<RwLock<HashMap<Uuid, PendingOperation>>>,
}

#[derive(Debug, Clone)]
struct PendingOperation {
    operation_type: String,
    vault_id: Uuid,
    amount: u64,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl CPIManager {
    pub fn new(
        vault_manager: Arc<VaultManager>,
        transaction_builder: Arc<TransactionBuilder>,
        transaction_submitter: Arc<TransactionSubmitter>,
        authority_keypair: Arc<Keypair>,
    ) -> Self {
        Self {
            vault_manager,
            transaction_builder,
            transaction_submitter,
            authority_keypair,
            pending_operations: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// Lock collateral for trading position
    pub async fn lock_collateral(&self, vault_id: Uuid, amount: u64, operation_id: Uuid) -> Result<String> {
        info!("Locking collateral: vault={}, amount={}, operation={}", vault_id, amount, operation_id);
        
        // Validate vault exists and has sufficient balance
        let vault = self.vault_manager.get_vault_by_id(vault_id).await?;
        if vault.available_balance < amount as i64 {
            return Err(VaultError::InsufficientBalance {
                available: vault.available_balance as u64,
                required: amount,
            });
        }
        
        // Check for duplicate operations
        if self.is_operation_pending(operation_id).await {
            return Err(VaultError::ConcurrentConflict(format!(
                "Operation {} already in progress", operation_id
            )));
        }
        
        // Add to pending operations
        self.add_pending_operation(operation_id, "lock", vault_id, amount).await;
        
        // Build and submit transaction
        let vault_pubkey = Pubkey::from_str(&vault.vault_pubkey)
            .map_err(|_| VaultError::ValidationError("Invalid vault pubkey".to_string()))?;
        
        let built_tx = self.transaction_builder
            .build_lock_collateral_tx(vault_pubkey, amount, &self.authority_keypair)
            .await?;
        
        // Create transaction record
        let tx_record = self.vault_manager.transaction_manager()
            .create_transaction(vault_id, TransactionType::Lock, amount as i64, None)
            .await?;
        
        // Submit transaction
        let result = self.submit_and_confirm(built_tx, tx_record.id).await;
        
        // Remove from pending operations
        self.remove_pending_operation(operation_id).await;
        
        match result {
            Ok(signature) => {
                info!("Collateral locked successfully: {}", signature);
                
                // Update vault balances
                let new_locked = vault.locked_balance + amount as i64;
                let new_available = vault.available_balance - amount as i64;
                
                self.vault_manager.update_balances(
                    vault_id,
                    vault.total_balance,
                    new_locked,
                    new_available,
                    Some(tx_record.id),
                    "cp_manager",
                ).await?;
                
                Ok(signature)
            }
            Err(e) => {
                error!("Failed to lock collateral: {}", e);
                Err(e)
            }
        }
    }
    
    /// Unlock collateral when position is closed
    pub async fn unlock_collateral(&self, vault_id: Uuid, amount: u64, operation_id: Uuid) -> Result<String> {
        info!("Unlocking collateral: vault={}, amount={}, operation={}", vault_id, amount, operation_id);
        
        // Validate vault exists and has sufficient locked balance
        let vault = self.vault_manager.get_vault_by_id(vault_id).await?;
        if vault.locked_balance < amount as i64 {
            return Err(VaultError::InsufficientBalance {
                available: vault.locked_balance as u64,
                required: amount,
            });
        }
        
        // Check for duplicate operations
        if self.is_operation_pending(operation_id).await {
            return Err(VaultError::ConcurrentConflict(format!(
                "Operation {} already in progress", operation_id
            )));
        }
        
        // Add to pending operations
        self.add_pending_operation(operation_id, "unlock", vault_id, amount).await;
        
        // Build and submit transaction
        let vault_pubkey = Pubkey::from_str(&vault.vault_pubkey)
            .map_err(|_| VaultError::ValidationError("Invalid vault pubkey".to_string()))?;
        
        let built_tx = self.transaction_builder
            .build_unlock_collateral_tx(vault_pubkey, amount, &self.authority_keypair)
            .await?;
        
        // Create transaction record
        let tx_record = self.vault_manager.transaction_manager()
            .create_transaction(vault_id, TransactionType::Unlock, amount as i64, None)
            .await?;
        
        // Submit transaction
        let result = self.submit_and_confirm(built_tx, tx_record.id).await;
        
        // Remove from pending operations
        self.remove_pending_operation(operation_id).await;
        
        match result {
            Ok(signature) => {
                info!("Collateral unlocked successfully: {}", signature);
                
                // Update vault balances
                let new_locked = vault.locked_balance - amount as i64;
                let new_available = vault.available_balance + amount as i64;
                
                self.vault_manager.update_balances(
                    vault_id,
                    vault.total_balance,
                    new_locked,
                    new_available,
                    Some(tx_record.id),
                    "cp_manager",
                ).await?;
                
                Ok(signature)
            }
            Err(e) => {
                error!("Failed to unlock collateral: {}", e);
                Err(e)
            }
        }
    }
    
    /// Transfer collateral between vaults (for settlement)
    pub async fn transfer_collateral(
        &self,
        source_vault_id: Uuid,
        destination_vault_id: Uuid,
        amount: u64,
        operation_id: Uuid,
    ) -> Result<String> {
        info!("Transferring collateral: source={}, dest={}, amount={}, operation={}", 
              source_vault_id, destination_vault_id, amount, operation_id);
        
        // Validate both vaults exist and source has sufficient locked balance
        let source_vault = self.vault_manager.get_vault_by_id(source_vault_id).await?;
        let destination_vault = self.vault_manager.get_vault_by_id(destination_vault_id).await?;
        
        if source_vault.locked_balance < amount as i64 {
            return Err(VaultError::InsufficientBalance {
                available: source_vault.locked_balance as u64,
                required: amount,
            });
        }
        
        // Check for duplicate operations
        if self.is_operation_pending(operation_id).await {
            return Err(VaultError::ConcurrentConflict(format!(
                "Operation {} already in progress", operation_id
            )));
        }
        
        // Add to pending operations
        self.add_pending_operation(operation_id, "transfer", source_vault_id, amount).await;
        
        // Build and submit transaction
        let source_vault_pubkey = Pubkey::from_str(&source_vault.vault_pubkey)
            .map_err(|_| VaultError::ValidationError("Invalid source vault pubkey".to_string()))?;
        let destination_vault_pubkey = Pubkey::from_str(&destination_vault.vault_pubkey)
            .map_err(|_| VaultError::ValidationError("Invalid destination vault pubkey".to_string()))?;
        
        let built_tx = self.transaction_builder
            .build_transfer_collateral_tx(
                source_vault_pubkey,
                destination_vault_pubkey,
                amount,
                &self.authority_keypair,
            )
            .await?;
        
        // Create transaction records for both vaults
        let source_tx_record = self.vault_manager.transaction_manager()
            .create_transaction(source_vault_id, TransactionType::Transfer, -(amount as i64), None)
            .await?;
        
        let destination_tx_record = self.vault_manager.transaction_manager()
            .create_transaction(destination_vault_id, TransactionType::Transfer, amount as i64, None)
            .await?;
        
        // Submit transaction
        let result = self.submit_and_confirm(built_tx, source_tx_record.id).await;
        
        // Remove from pending operations
        self.remove_pending_operation(operation_id).await;
        
        match result {
            Ok(signature) => {
                info!("Collateral transferred successfully: {}", signature);
                
                // Update source vault balances (reduce locked and total)
                let source_new_locked = source_vault.locked_balance - amount as i64;
                let source_new_total = source_vault.total_balance - amount as i64;
                
                self.vault_manager.update_balances(
                    source_vault_id,
                    source_new_total,
                    source_new_locked,
                    source_vault.available_balance,
                    Some(source_tx_record.id),
                    "cp_manager",
                ).await?;
                
                // Update destination vault balances (increase available and total)
                let destination_new_total = destination_vault.total_balance + amount as i64;
                let destination_new_available = destination_vault.available_balance + amount as i64;
                
                self.vault_manager.update_balances(
                    destination_vault_id,
                    destination_new_total,
                    destination_vault.locked_balance,
                    destination_new_available,
                    Some(destination_tx_record.id),
                    "cp_manager",
                ).await?;
                
                Ok(signature)
            }
            Err(e) => {
                error!("Failed to transfer collateral: {}", e);
                
                // Mark destination transaction as failed
                self.vault_manager.transaction_manager()
                    .update_transaction_status(destination_tx_record.id, TransactionStatus::Failed, Some(e.to_string()))
                    .await?;
                
                Err(e)
            }
        }
    }
    
    /// Submit transaction and wait for confirmation
    async fn submit_and_confirm(&self, built_tx: BuiltTransaction, tx_record_id: Uuid) -> Result<String> {
        // Submit transaction
        let signature = self.transaction_submitter.submit_transaction(built_tx.transaction, tx_record_id).await?;
        
        // Update transaction record with signature
        self.vault_manager.transaction_manager()
            .update_transaction_status(tx_record_id, TransactionStatus::Confirmed, None)
            .await?;
        
        Ok(signature)
    }
    
    /// Check if operation is already pending
    async fn is_operation_pending(&self, operation_id: Uuid) -> bool {
        let pending = self.pending_operations.read().await;
        pending.contains_key(&operation_id)
    }
    
    /// Add pending operation
    async fn add_pending_operation(&self, operation_id: Uuid, operation_type: &str, vault_id: Uuid, amount: u64) {
        let mut pending = self.pending_operations.write().await;
        let now = Utc::now();
        let expires_at = now + Duration::minutes(5); // Operations expire after 5 minutes
        
        pending.insert(operation_id, PendingOperation {
            operation_type: operation_type.to_string(),
            vault_id,
            amount,
            created_at: now,
            expires_at,
        });
    }
    
    /// Remove pending operation
    async fn remove_pending_operation(&self, operation_id: Uuid) {
        let mut pending = self.pending_operations.write().await;
        pending.remove(&operation_id);
    }
    
    /// Cleanup expired pending operations
    pub async fn cleanup_expired_operations(&self) {
        let mut pending = self.pending_operations.write().await;
        let now = Utc::now();
        
        pending.retain(|_, op| op.expires_at > now);
        
        let remaining = pending.len();
        if remaining > 0 {
            info!("Cleaned up expired operations. {} operations remaining.", remaining);
        }
    }
    
    /// Get pending operations count
    pub async fn get_pending_operations_count(&self) -> usize {
        let pending = self.pending_operations.read().await;
        pending.len()
    }
}