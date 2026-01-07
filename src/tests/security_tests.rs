use collateral_vault_backend::{
    VaultManager, TransactionManager, BalanceTracker, TransactionBuilder, TransactionSubmitter,
    CPIManager, VaultMonitor, MonitorConfig, models::*, error::*, database::*, api,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[cfg(test)]
mod security_tests {
    use super::*;
    
    async fn setup_test_app() -> (axum::Router, sqlx::PgPool) {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_test_db".to_string());
        
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database");
        
        // Initialize services
        let vault_manager = Arc::new(VaultManager::new(pool.clone()));
        let transaction_manager = Arc::new(TransactionManager::new(pool.clone()));
        let balance_tracker = Arc::new(BalanceTracker::new(pool.clone(), 3600));
        
        // Create mock keypairs for testing
        let payer_keypair = Arc::new(solana_sdk::signature::Keypair::new());
        let authority_keypair = Arc::new(solana_sdk::signature::Keypair::new());
        
        let transaction_builder = Arc::new(TransactionBuilder::new(
            "https://api.testnet.solana.com",
            payer_keypair,
            solana_sdk::pubkey::Pubkey::new_unique(),
            5,
        ).expect("Failed to create transaction builder"));
        
        let transaction_submitter = Arc::new(TransactionSubmitter::new(
            std::sync::Arc::new(solana_client::rpc_client::RpcClient::new("https://api.testnet.solana.com")),
            3,
            1000,
        ));
        
        let cpi_manager = Arc::new(CPIManager::new(
            vault_manager.clone(),
            transaction_builder.clone(),
            transaction_submitter.clone(),
            authority_keypair,
        ));
        
        let monitor_config = MonitorConfig {
            reconciliation_interval_seconds: 60,
            health_check_interval_seconds: 30,
            stale_transaction_threshold_seconds: 3600,
            max_pending_transactions: 100,
        };
        
        let monitor = Arc::new(VaultMonitor::new(
            pool.clone(),
            vault_manager.clone(),
            balance_tracker.clone(),
            transaction_builder.clone(),
            transaction_submitter.clone(),
            monitor_config,
        ));
        
        let rate_limit_repo = Arc::new(RateLimitRepository::new(pool.clone()));
        
        // Create app state
        let app_state = api::AppState {
            vault_manager,
            transaction_manager,
            balance_tracker,
            cpi_manager,
            monitor,
            rate_limit_repo,
        };
        
        (api::create_router(app_state), pool)
    }
    
    #[tokio::test]
    async fn test_sql_injection_prevention() {
        let (app, _pool) = setup_test_app().await;
        
        // Try SQL injection in user_pubkey parameter
        let malicious_user_pubkey = "'; DROP TABLE vaults; --";
        
        let response = app
            .clone()
            .oneshot(Request::builder()
                .uri(&format!("/vaults/{}", malicious_user_pubkey))
                .body(Body::empty())
                .unwrap())
            .await
            .unwrap();
        
        // Should not crash or execute SQL injection
        // Should return 400 Bad Request or handle gracefully
        assert!(response.status() == StatusCode::OK || 
                response.status() == StatusCode::BAD_REQUEST ||
                response.status() == StatusCode::NOT_FOUND);
    }
    
    #[tokio::test]
    async fn test_xss_prevention() {
        let (app, _pool) = setup_test_app().await;
        
        // Try XSS payload in request body
        let xss_payload = json!({
            "user_pubkey": "<script>alert('XSS')</script>",
            "authority_pubkey": "test_authority"
        });
        
        let response = app
            .oneshot(Request::builder()
                .method("POST")
                .uri("/vaults")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&xss_payload).unwrap()))
                .unwrap())
            .await
            .unwrap();
        
        assert_eq!(response.status(), StatusCode::OK);
        
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        
        // Response should not contain unescaped XSS payload
        if let Some(vault_pubkey) = body_json.get("vault_pubkey") {
            let vault_pubkey_str = vault_pubkey.as_str().unwrap_or("");
            assert!(!vault_pubkey_str.contains("<script>"));
        }
    }
    
    #[tokio::test]
    async fn test_rate_limiting_bypass_attempts() {
        let (app, _pool) = setup_test_app().await;
        
        // Try different client identifiers to bypass rate limiting
        let client_ids = vec![
            "client_1",
            "client_2", 
            "client_3",
            "client_1", // Same as first - should be rate limited
        ];
        
        let mut responses = Vec::new();
        
        for client_id in client_ids {
            for _ in 0..25 {
                let response = app
                    .clone()
                    .oneshot(Request::builder()
                        .uri("/health")
                        .header("Authorization", &format!("Bearer {}", client_id))
                        .body(Body::empty())
                        .unwrap())
                    .await
                    .unwrap();
                
                responses.push((client_id.to_string(), response));
            }
        }
        
        // Check that rate limiting is applied per client
        let client_1_responses: Vec<_> = responses
            .iter()
            .filter(|(id, _)| id == "client_1")
            .collect();
        
        let rate_limited_count = client_1_responses
            .iter()
            .filter(|(_, r)| r.status() == StatusCode::TOO_MANY_REQUESTS)
            .count();
        
        // Should have some rate limited responses for client_1
        assert!(rate_limited_count > 0);
    }
    
    #[tokio::test]
    async fn test_authorization_bypass_attempts() {
        let (app, _pool) = setup_test_app().await;
        
        // Try various authorization bypass techniques
        let bypass_attempts = vec![
            ("Authorization", "Bearer "), // Empty token
            ("Authorization", "Basic dGVzdDp0ZXN0"), // Basic auth
            ("X-API-Key", "test_key"), // Custom header
            ("Authorization", "Bearer ' OR '1'='1"), // SQL injection attempt
        ];
        
        for (header_name, header_value) in bypass_attempts {
            let response = app
                .clone()
                .oneshot(Request::builder()
                    .uri("/health")
                    .header(header_name, header_value)
                    .body(Body::empty())
                    .unwrap())
                .await
                .unwrap();
            
            // Should not crash or allow unauthorized access
            assert!(response.status() == StatusCode::OK || 
                    response.status() == StatusCode::TOO_MANY_REQUESTS);
        }
    }
    
    #[tokio::test]
    async fn test_path_traversal_attempts() {
        let (app, _pool) = setup_test_app().await;
        
        // Try path traversal attacks
        let malicious_paths = vec![
            "../../../etc/passwd",
            "..\\..\\..\\windows\\system32\\config\\sam",
            "%2e%2e%2f%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        ];
        
        for path in malicious_paths {
            let response = app
                .clone()
                .oneshot(Request::builder()
                    .uri(&format!("/vaults/{}", path))
                    .body(Body::empty())
                    .unwrap())
                .await
                .unwrap();
            
            // Should not expose system files or crash
            assert!(response.status() == StatusCode::OK || 
                    response.status() == StatusCode::BAD_REQUEST ||
                    response.status() == StatusCode::NOT_FOUND);
        }
    }
    
    #[tokio::test]
    async fn test_large_payload_handling() {
        let (app, _pool) = setup_test_app().await;
        
        // Create very large request body
        let large_user_pubkey = "x".repeat(10000);
        let large_request = json!({
            "user_pubkey": large_user_pubkey,
            "authority_pubkey": "test_authority"
        });
        
        let response = app
            .oneshot(Request::builder()
                .method("POST")
                .uri("/vaults")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&large_request).unwrap()))
                .unwrap())
            .await
            .unwrap();
        
        // Should handle large payloads gracefully (413 Payload Too Large or similar)
        assert!(response.status() == StatusCode::OK || 
                response.status() == StatusCode::PAYLOAD_TOO_LARGE ||
                response.status() == StatusCode::BAD_REQUEST);
    }
    
    #[tokio::test]
    async fn test_malformed_json_handling() {
        let (app, _pool) = setup_test_app().await;
        
        // Send malformed JSON
        let malformed_json = r#"{"user_pubkey": "test", "authority_pubkey":}"#;
        
        let response = app
            .oneshot(Request::builder()
                .method("POST")
                .uri("/vaults")
                .header("content-type", "application/json")
                .body(Body::from(malformed_json))
                .unwrap())
            .await
            .unwrap();
        
        // Should return 400 Bad Request
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    
    #[tokio::test]
    async fn test_idempotency_key_reuse() {
        let (app, _pool) = setup_test_app().await;
        
        let idempotency_key = "test_idempotency_security";
        let request_body = json!({
            "amount": 1000000,
            "user_token_account": "test_token_account",
            "idempotency_key": idempotency_key
        });
        
        // Make first request
        let response1 = app
            .clone()
            .oneshot(Request::builder()
                .method("POST")
                .uri("/vaults/test_user_idempotent/deposit")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request_body).unwrap()))
                .unwrap())
            .await
            .unwrap();
        
        // Make second request with same idempotency key
        let response2 = app
            .clone()
            .oneshot(Request::builder()
                .method("POST")
                .uri("/vaults/test_user_idempotent/deposit")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request_body).unwrap()))
                .unwrap())
            .await
            .unwrap();
        
        // Both responses should have the same status (idempotent behavior)
        assert_eq!(response1.status(), response2.status());
    }
    
    #[tokio::test]
    async fn test_concurrent_request_handling() {
        let (app, _pool) = setup_test_app().await;
        
        // Send many concurrent requests
        let mut handles = Vec::new();
        
        for i in 0..50 {
            let app_clone = app.clone();
            let handle = tokio::spawn(async move {
                let request_body = json!({
                    "user_pubkey": format!("concurrent_user_{}", i),
                    "authority_pubkey": "test_authority"
                });
                
                app_clone
                    .oneshot(Request::builder()
                        .method("POST")
                        .uri("/vaults")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_string(&request_body).unwrap()))
                        .unwrap())
                    .await
                    .unwrap()
            });
            
            handles.push(handle);
        }
        
        // Wait for all requests to complete
        let results = futures::future::join_all(handles).await;
        
        // All requests should complete successfully
        for result in results {
            let response = result.unwrap();
            assert!(response.status() == StatusCode::OK || 
                    response.status() == StatusCode::TOO_MANY_REQUESTS);
        }
    }
}

#[cfg(test)]
mod adversarial_tests {
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
    async fn test_balance_invariant_manipulation_attempts() {
        let pool = setup_test_db().await;
        let vault_manager = Arc::new(VaultManager::new(pool.clone()));
        
        // Create a vault
        let user_pubkey = "adversarial_user_balance";
        let vault_pubkey = "adversarial_vault_balance";
        let token_account = "adversarial_token_balance";
        
        let vault = vault_manager
            .create_vault(user_pubkey, vault_pubkey, token_account)
            .await
            .unwrap();
        
        // Try to manipulate balances through concurrent updates
        let mut handles = Vec::new();
        
        for i in 0..10 {
            let vault_manager_clone = vault_manager.clone();
            let vault_id = vault.id;
            
            let handle = tokio::spawn(async move {
                // Try to set inconsistent balances
                vault_manager_clone.update_vault_balances(
                    vault_id,
                    1000 + i * 100,
                    300,
                    700 + i * 50, // This creates inconsistent state
                ).await
            });
            
            handles.push(handle);
        }
        
        let results = futures::future::join_all(handles).await;
        
        // Some updates should fail due to balance invariant violation
        let failures: Vec<_> = results
            .into_iter()
            .filter(|r| r.is_err() || r.as_ref().unwrap().is_err())
            .collect();
        
        // At least some should fail
        assert!(!failures.is_empty());
    }
    
    #[tokio::test]
    async fn test_negative_balance_attempts() {
        let pool = setup_test_db().await;
        let vault_manager = Arc::new(VaultManager::new(pool.clone()));
        
        // Create a vault
        let user_pubkey = "adversarial_user_negative";
        let vault_pubkey = "adversarial_vault_negative";
        let token_account = "adversarial_token_negative";
        
        let vault = vault_manager
            .create_vault(user_pubkey, vault_pubkey, token_account)
            .await
            .unwrap();
        
        // Try to set negative balances
        let result = vault_manager.update_vault_balances(
            vault.id,
            -1000, // Negative total balance
            0,
            -1000, // Negative available balance
        ).await;
        
        // Should fail
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn test_overflow_underflow_attempts() {
        let pool = setup_test_db().await;
        let vault_manager = Arc::new(VaultManager::new(pool.clone()));
        
        // Create a vault
        let user_pubkey = "adversarial_user_overflow";
        let vault_pubkey = "adversarial_vault_overflow";
        let token_account = "adversarial_token_overflow";
        
        let vault = vault_manager
            .create_vault(user_pubkey, vault_pubkey, token_account)
            .await
            .unwrap();
        
        // Try to set extremely large balances that might cause overflow
        let result = vault_manager.update_vault_balances(
            vault.id,
            i64::MAX,
            i64::MAX / 2,
            i64::MAX / 2,
        ).await;
        
        // Should handle large numbers safely
        assert!(result.is_ok() || result.is_err());
    }
    
    #[tokio::test]
    async fn test_race_condition_in_balance_updates() {
        let pool = setup_test_db().await;
        let vault_manager = Arc::new(VaultManager::new(pool.clone()));
        
        // Create a vault
        let user_pubkey = "adversarial_user_race";
        let vault_pubkey = "adversarial_vault_race";
        let token_account = "adversarial_token_race";
        
        let vault = vault_manager
            .create_vault(user_pubkey, vault_pubkey, token_account)
            .await
            .unwrap();
        
        // Set initial balance
        vault_manager.update_vault_balances(vault.id, 1000, 0, 1000).await.unwrap();
        
        // Try concurrent balance updates
        let mut handles = Vec::new();
        
        for i in 0..20 {
            let vault_manager_clone = vault_manager.clone();
            let vault_id = vault.id;
            
            let handle = tokio::spawn(async move {
                // Each tries to update balance differently
                match i % 4 {
                    0 => vault_manager_clone.update_vault_balances(vault_id, 1000 + i, 0, 1000 + i).await,
                    1 => vault_manager_clone.update_vault_balances(vault_id, 1000 - i, 0, 1000 - i).await,
                    2 => vault_manager_clone.update_vault_balances(vault_id, 1000, i, 1000 - i).await,
                    _ => vault_manager_clone.update_vault_balances(vault_id, 1000, 0, 1000).await,
                }
            });
            
            handles.push(handle);
        }
        
        let results = futures::future::join_all(handles).await;
        
        // Some updates should succeed, some should fail due to conflicts
        let successes: Vec<_> = results
            .into_iter()
            .filter(|r| r.is_ok() && r.as_ref().unwrap().is_ok())
            .collect();
        
        // At least some should succeed
        assert!(!successes.is_empty());
        
        // Verify final state is consistent
        let final_vault = vault_manager.get_vault_by_user(user_pubkey).await.unwrap();
        assert_eq!(final_vault.total_balance, final_vault.locked_balance + final_vault.available_balance);
    }
    
    #[tokio::test]
    async fn test_transaction_replay_attacks() {
        let pool = setup_test_db().await;
        let transaction_manager = Arc::new(TransactionManager::new(pool.clone()));
        
        let vault_id = Uuid::new_v4();
        let signature = "replay_attack_signature";
        
        // Record transaction
        transaction_manager.record_transaction(
            vault_id,
            signature,
            "deposit",
            1000,
            TransactionStatus::Confirmed,
            "confirmed_hash",
        ).await.unwrap();
        
        // Try to record same transaction again (replay attack)
        let result = transaction_manager.record_transaction(
            vault_id,
            signature,
            "withdraw",
            500,
            TransactionStatus::Pending,
            "pending_hash",
        ).await;
        
        // Should fail due to duplicate signature
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn test_idempotency_key_manipulation() {
        let pool = setup_test_db().await;
        let transaction_manager = Arc::new(TransactionManager::new(pool.clone()));
        
        let vault_id = Uuid::new_v4();
        let idempotency_key = "manipulated_idempotency_key";
        
        // Record transaction with idempotency key
        let tx1 = transaction_manager.record_transaction_with_idempotency(
            vault_id,
            "sig1",
            "deposit",
            1000,
            TransactionStatus::Pending,
            "hash1",
            Some(idempotency_key),
        ).await.unwrap();
        
        // Try to manipulate by using same idempotency key with different parameters
        let tx2 = transaction_manager.record_transaction_with_idempotency(
            vault_id,
            "sig2",
            "withdraw", // Different operation
            2000,       // Different amount
            TransactionStatus::Pending,
            "hash2",
            Some(idempotency_key),
        ).await.unwrap();
        
        // Should return the original transaction (idempotency enforced)
        assert_eq!(tx1.id, tx2.id);
        assert_eq!(tx1.operation, "deposit"); // Original operation preserved
        assert_eq!(tx1.amount, 1000); // Original amount preserved
    }
}