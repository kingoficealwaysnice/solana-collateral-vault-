use collateral_vault_backend::{
    VaultManager, TransactionManager, BalanceTracker, TransactionBuilder, TransactionSubmitter,
    CPIManager, VaultMonitor, MonitorConfig, models::*, error::*, database::*,
};
use sqlx::postgres::PgPoolOptions;
use solana_sdk::signature::{Keypair, Signer};
use std::sync::Arc;
use uuid::Uuid;

#[cfg(test)]
mod vault_manager_tests {
    use super::*;
    
    async fn setup_test_db() -> sqlx::PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_test_db".to_string());
        
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database")
    }
    
    #[tokio::test]
    async fn test_create_vault() {
        let pool = setup_test_db().await;
        let vault_manager = VaultManager::new(pool.clone());
        
        let user_pubkey = "test_user_123";
        let vault_pubkey = "test_vault_456";
        let token_account = "test_token_789";
        
        let result = vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await;
        assert!(result.is_ok());
        
        let vault = result.unwrap();
        assert_eq!(vault.user_pubkey, user_pubkey);
        assert_eq!(vault.vault_pubkey, vault_pubkey);
        assert_eq!(vault.token_account_pubkey, token_account);
        assert_eq!(vault.total_balance, 0);
        assert_eq!(vault.locked_balance, 0);
        assert_eq!(vault.available_balance, 0);
        assert!(vault.is_active);
    }
    
    #[tokio::test]
    async fn test_get_vault_by_user() {
        let pool = setup_test_db().await;
        let vault_manager = VaultManager::new(pool.clone());
        
        let user_pubkey = "test_user_get";
        let vault_pubkey = "test_vault_get";
        let token_account = "test_token_get";
        
        // Create vault first
        vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await.unwrap();
        
        // Get vault by user
        let result = vault_manager.get_vault_by_user(user_pubkey).await;
        assert!(result.is_ok());
        
        let vault = result.unwrap();
        assert_eq!(vault.user_pubkey, user_pubkey);
        assert_eq!(vault.vault_pubkey, vault_pubkey);
    }
    
    #[tokio::test]
    async fn test_update_vault_balances() {
        let pool = setup_test_db().await;
        let vault_manager = VaultManager::new(pool.clone());
        
        let user_pubkey = "test_user_balances";
        let vault_pubkey = "test_vault_balances";
        let token_account = "test_token_balances";
        
        // Create vault
        let vault = vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await.unwrap();
        
        // Update balances
        let result = vault_manager.update_vault_balances(
            vault.id,
            1000, // total
            300,  // locked
            700,  // available
        ).await;
        
        assert!(result.is_ok());
        
        // Verify updated vault
        let updated_vault = vault_manager.get_vault_by_user(user_pubkey).await.unwrap();
        assert_eq!(updated_vault.total_balance, 1000);
        assert_eq!(updated_vault.locked_balance, 300);
        assert_eq!(updated_vault.available_balance, 700);
    }
    
    #[tokio::test]
    async fn test_balance_invariant_enforcement() {
        let pool = setup_test_db().await;
        let vault_manager = VaultManager::new(pool.clone());
        
        let user_pubkey = "test_user_invariant";
        let vault_pubkey = "test_vault_invariant";
        let token_account = "test_token_invariant";
        
        // Create vault
        let vault = vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await.unwrap();
        
        // Try to update with invalid balance invariant (total != locked + available)
        let result = vault_manager.update_vault_balances(
            vault.id,
            1000, // total
            300,  // locked
            600,  // available (should be 700)
        ).await;
        
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VaultError::BalanceInvariantViolation));
    }
}

#[cfg(test)]
mod balance_tracker_tests {
    use super::*;
    
    async fn setup_test_db() -> sqlx::PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_test_db".to_string());
        
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database")
    }
    
    #[tokio::test]
    async fn test_record_balance_snapshot() {
        let pool = setup_test_db().await;
        let balance_tracker = BalanceTracker::new(pool.clone(), 3600);
        
        let user_pubkey = "test_user_snapshot";
        let vault_pubkey = "test_vault_snapshot";
        let token_account = "test_token_snapshot";
        
        // Create vault first
        let vault_manager = VaultManager::new(pool.clone());
        let vault = vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await.unwrap();
        
        // Record balance snapshot
        let result = balance_tracker.record_balance_snapshot(
            vault.id,
            1000, // total
            300,  // locked
            700,  // available
            "test_tx_123",
        ).await;
        
        assert!(result.is_ok());
        
        let snapshot = result.unwrap();
        assert_eq!(snapshot.vault_id, vault.id);
        assert_eq!(snapshot.total_balance, 1000);
        assert_eq!(snapshot.locked_balance, 300);
        assert_eq!(snapshot.available_balance, 700);
        assert_eq!(snapshot.transaction_signature, "test_tx_123");
    }
    
    #[tokio::test]
    async fn test_get_balance_history() {
        let pool = setup_test_db().await;
        let balance_tracker = BalanceTracker::new(pool.clone(), 3600);
        
        let user_pubkey = "test_user_history";
        let vault_pubkey = "test_vault_history";
        let token_account = "test_token_history";
        
        // Create vault
        let vault_manager = VaultManager::new(pool.clone());
        let vault = vault_manager.create_vault(user_pubkey, vault_pubkey, token_account).await.unwrap();
        
        // Record multiple snapshots
        for i in 0..5 {
            balance_tracker.record_balance_snapshot(
                vault.id,
                1000 + i * 100,
                300 + i * 50,
                700 + i * 50,
                &format!("test_tx_{}", i),
            ).await.unwrap();
        }
        
        // Get balance history
        let history = balance_tracker.get_balance_history(vault.id, 10).await.unwrap();
        assert_eq!(history.len(), 5);
        
        // Verify chronological order (newest first)
        assert_eq!(history[0].total_balance, 1400);
        assert_eq!(history[4].total_balance, 1000);
    }
}

#[cfg(test)]
mod transaction_manager_tests {
    use super::*;
    
    async fn setup_test_db() -> sqlx::PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_test_db".to_string());
        
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database")
    }
    
    #[tokio::test]
    async fn test_record_transaction() {
        let pool = setup_test_db().await;
        let transaction_manager = TransactionManager::new(pool.clone());
        
        let vault_id = Uuid::new_v4();
        let tx_signature = "test_signature_123";
        let operation = "deposit";
        let amount = 1000i64;
        
        let result = transaction_manager.record_transaction(
            vault_id,
            tx_signature,
            operation,
            amount,
            TransactionStatus::Pending,
            "test_hash",
        ).await;
        
        assert!(result.is_ok());
        
        let transaction = result.unwrap();
        assert_eq!(transaction.vault_id, vault_id);
        assert_eq!(transaction.signature, tx_signature);
        assert_eq!(transaction.operation, operation);
        assert_eq!(transaction.amount, amount);
        assert!(matches!(transaction.status, TransactionStatus::Pending));
    }
    
    #[tokio::test]
    async fn test_update_transaction_status() {
        let pool = setup_test_db().await;
        let transaction_manager = TransactionManager::new(pool.clone());
        
        let vault_id = Uuid::new_v4();
        let tx_signature = "test_signature_update";
        
        // Record transaction
        let transaction = transaction_manager.record_transaction(
            vault_id,
            tx_signature,
            "withdraw",
            500,
            TransactionStatus::Pending,
            "test_hash",
        ).await.unwrap();
        
        // Update status
        let result = transaction_manager.update_transaction_status(
            transaction.id,
            TransactionStatus::Confirmed,
            "confirmed_hash",
        ).await;
        
        assert!(result.is_ok());
        
        // Verify updated transaction
        let updated = transaction_manager.get_transaction_by_signature(tx_signature).await.unwrap();
        assert!(matches!(updated.status, TransactionStatus::Confirmed));
        assert_eq!(updated.confirmation_hash, Some("confirmed_hash".to_string()));
    }
    
    #[tokio::test]
    async fn test_idempotency_key_handling() {
        let pool = setup_test_db().await;
        let transaction_manager = TransactionManager::new(pool.clone());
        
        let vault_id = Uuid::new_v4();
        let idempotency_key = "test_idempotency_key";
        
        // Record first transaction with idempotency key
        let result1 = transaction_manager.record_transaction_with_idempotency(
            vault_id,
            "sig1",
            "deposit",
            1000,
            TransactionStatus::Pending,
            "hash1",
            Some(idempotency_key),
        ).await;
        
        assert!(result1.is_ok());
        
        // Try to record another transaction with same idempotency key
        let result2 = transaction_manager.record_transaction_with_idempotency(
            vault_id,
            "sig2",
            "deposit",
            1000,
            TransactionStatus::Pending,
            "hash2",
            Some(idempotency_key),
        ).await;
        
        // Should return the first transaction (idempotency)
        assert!(result2.is_ok());
        assert_eq!(result1.unwrap().id, result2.unwrap().id);
    }
}

#[cfg(test)]
mod database_repository_tests {
    use super::*;
    
    async fn setup_test_db() -> sqlx::PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_test_db".to_string());
        
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database")
    }
    
    #[tokio::test]
    async fn test_rate_limit_repository() {
        let pool = setup_test_db().await;
        let rate_limit_repo = RateLimitRepository::new(pool);
        
        let bucket_key = "test_client_123";
        
        // Consume tokens within limit
        for i in 0..50 {
            let result = rate_limit_repo.consume_tokens(bucket_key, 1, 100, 10).await;
            assert!(result.is_ok());
            let rate_limit_result = result.unwrap();
            assert!(rate_limit_result.allowed);
            assert_eq!(rate_limit_result.remaining_tokens, 99 - i);
        }
        
        // Try to consume more tokens than available
        let result = rate_limit_repo.consume_tokens(bucket_key, 60, 100, 10).await;
        assert!(result.is_ok());
        let rate_limit_result = result.unwrap();
        assert!(!rate_limit_result.allowed);
        assert_eq!(rate_limit_result.remaining_tokens, 50);
    }
    
    #[tokio::test]
    async fn test_audit_log_repository() {
        let pool = setup_test_db().await;
        let audit_repo = AuditRepository::new(pool);
        
        let vault_id = Uuid::new_v4();
        let user_pubkey = "test_user_audit";
        let action = "vault_created";
        let details = serde_json::json!({"initial_balance": 0});
        
        let result = audit_repo.log_action(
            vault_id,
            user_pubkey,
            action,
            details.clone(),
        ).await;
        
        assert!(result.is_ok());
        
        let audit_log = result.unwrap();
        assert_eq!(audit_log.vault_id, vault_id);
        assert_eq!(audit_log.user_pubkey, user_pubkey);
        assert_eq!(audit_log.action, action);
        assert_eq!(audit_log.details, details);
    }
}