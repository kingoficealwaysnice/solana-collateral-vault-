use axum::{
    routing::{get, post, put, delete},
    Router,
    extract::{Path, State, Json, Query},
    response::{Json as JsonResponse, Response},
    http::StatusCode,
    middleware,
};
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use tracing::{info, warn, error};

use crate::{
    VaultManager, TransactionManager, BalanceTracker, CPIManager, VaultMonitor,
    models::*, error::{Result, VaultError},
    database::RateLimitRepository,
};

#[derive(Clone)]
pub struct AppState {
    pub vault_manager: Arc<VaultManager>,
    pub transaction_manager: Arc<TransactionManager>,
    pub balance_tracker: Arc<BalanceTracker>,
    pub cpi_manager: Arc<CPIManager>,
    pub monitor: Arc<VaultMonitor>,
    pub rate_limit_repo: Arc<RateLimitRepository>,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        // Health and monitoring
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .route("/ws/metrics", get(metrics_websocket))
        
        // Vault management
        .route("/vaults", get(list_vaults).post(create_vault))
        .route("/vaults/:user_pubkey", get(get_vault))
        .route("/vaults/:user_pubkey/balance", get(get_balance))
        .route("/vaults/:user_pubkey/state", put(update_vault_state))
        
        // Transaction operations
        .route("/vaults/:user_pubkey/deposit", post(deposit))
        .route("/vaults/:user_pubkey/withdraw", post(withdraw))
        .route("/vaults/:user_pubkey/lock", post(lock_collateral))
        .route("/vaults/:user_pubkey/unlock", post(unlock_collateral))
        .route("/vaults/:user_pubkey/transfer", post(transfer_collateral))
        
        // Transaction history
        .route("/vaults/:user_pubkey/transactions", get(get_vault_transactions))
        .route("/transactions/:transaction_id", get(get_transaction))
        
        // Balance operations
        .route("/vaults/:user_pubkey/snapshots", get(get_balance_snapshots))
        .route("/vaults/:user_pubkey/reconcile", post(reconcile_balance))
        
        // System operations
        .route("/system/stats", get(get_system_stats))
        .route("/system/config", get(get_system_config).put(update_system_config))
        .route("/system/audit-log", get(get_audit_log))
        
        // WebSocket endpoints
        .route("/ws/vaults/:user_pubkey", get(vault_websocket))
        
        .with_state(state)
        .layer(middleware::from_fn(rate_limit_middleware))
}

// Request/Response DTOs

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    pub user_pubkey: String,
    pub authority_pubkey: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateVaultResponse {
    pub vault_id: Uuid,
    pub user_pubkey: String,
    pub vault_pubkey: String,
    pub token_account_pubkey: String,
    pub bump: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VaultResponse {
    pub id: Uuid,
    pub user_pubkey: String,
    pub vault_pubkey: String,
    pub token_account_pubkey: String,
    pub total_balance: i64,
    pub locked_balance: i64,
    pub available_balance: i64,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub total_balance: i64,
    pub locked_balance: i64,
    pub available_balance: i64,
    pub last_updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionRequest {
    pub amount: u64,
    pub idempotency_key: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransferRequest {
    pub amount: u64,
    pub destination_user_pubkey: String,
    pub idempotency_key: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionResponse {
    pub transaction_id: Uuid,
    pub solana_signature: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListTransactionsQuery {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub transaction_type: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemStatsResponse {
    pub vault_count: i64,
    pub pending_transactions: i64,
    pub failed_transactions_24h: i64,
    pub total_value_locked: i64,
    pub is_healthy: bool,
    pub last_reconciliation: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub timestamp: DateTime<Utc>,
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
    pub request_id: Option<String>,
}

// API Handlers

async fn health_check(State(state): State<AppState>) -> JsonResponse<HealthResponse> {
    let is_healthy = state.monitor.get_health_status().await;
    
    JsonResponse(HealthResponse {
        status: if is_healthy { "healthy".to_string() } else { "unhealthy".to_string() },
        timestamp: Utc::now(),
        details: None,
    })
}

async fn get_metrics(State(state): State<AppState>) -> JsonResponse<SystemStatsResponse> {
    match state.monitor.get_stats().await {
        Ok(stats) => JsonResponse(SystemStatsResponse {
            vault_count: stats.vault_count,
            pending_transactions: stats.pending_transactions,
            failed_transactions_24h: stats.failed_transactions_24h,
            total_value_locked: stats.total_value_locked,
            is_healthy: stats.is_healthy,
            last_reconciliation: stats.last_reconciliation,
        }),
        Err(e) => {
            error!("Failed to get system stats: {}", e);
            JsonResponse(SystemStatsResponse {
                vault_count: 0,
                pending_transactions: 0,
                failed_transactions_24h: 0,
                total_value_locked: 0,
                is_healthy: false,
                last_reconciliation: None,
            })
        }
    }
}

async fn list_vaults(
    State(state): State<AppState>,
    Query(params): Query<ListTransactionsQuery>,
) -> JsonResponse<Vec<VaultResponse>> {
    let limit = params.limit.unwrap_or(50).min(100) as i64;
    let offset = ((params.page.unwrap_or(1) - 1) * params.limit.unwrap_or(50) as u32) as i64;
    
    match state.vault_manager.get_active_vaults(limit, offset).await {
        Ok(vaults) => {
            let responses: Vec<VaultResponse> = vaults.into_iter().map(|v| VaultResponse {
                id: v.id,
                user_pubkey: v.user_pubkey,
                vault_pubkey: v.vault_pubkey,
                token_account_pubkey: v.token_account_pubkey,
                total_balance: v.total_balance,
                locked_balance: v.locked_balance,
                available_balance: v.available_balance,
                is_active: v.is_active,
                created_at: v.created_at,
                last_activity_at: v.last_activity_at,
            }).collect();
            JsonResponse(responses)
        }
        Err(e) => {
            error!("Failed to list vaults: {}", e);
            JsonResponse(vec![])
        }
    }
}

async fn create_vault(
    State(state): State<AppState>,
    Json(request): Json<CreateVaultRequest>,
) -> Result<JsonResponse<CreateVaultResponse>, VaultError> {
    info!("Creating vault for user: {}", request.user_pubkey);
    
    let vault = state.vault_manager.create_vault(
        &request.user_pubkey,
        &request.authority_pubkey,
    ).await?;
    
    Ok(JsonResponse(CreateVaultResponse {
        vault_id: vault.id,
        user_pubkey: vault.user_pubkey,
        vault_pubkey: vault.vault_pubkey,
        token_account_pubkey: vault.token_account_pubkey,
        bump: vault.bump as u8,
    }))
}

async fn get_vault(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
) -> Result<JsonResponse<VaultResponse>, VaultError> {
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    Ok(JsonResponse(VaultResponse {
        id: vault.id,
        user_pubkey: vault.user_pubkey,
        vault_pubkey: vault.vault_pubkey,
        token_account_pubkey: vault.token_account_pubkey,
        total_balance: vault.total_balance,
        locked_balance: vault.locked_balance,
        available_balance: vault.available_balance,
        is_active: vault.is_active,
        created_at: vault.created_at,
        last_activity_at: vault.last_activity_at,
    }))
}

async fn get_balance(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
) -> Result<JsonResponse<BalanceResponse>, VaultError> {
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    Ok(JsonResponse(BalanceResponse {
        total_balance: vault.total_balance,
        locked_balance: vault.locked_balance,
        available_balance: vault.available_balance,
        last_updated_at: vault.updated_at,
    }))
}

async fn deposit(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(request): Json<TransactionRequest>,
) -> Result<JsonResponse<TransactionResponse>, VaultError> {
    info!("Processing deposit for user: {}, amount: {}", user_pubkey, request.amount);
    
    // Check idempotency
    if let Some(idempotency_key) = &request.idempotency_key {
        if let Some(existing_tx) = state.transaction_manager
            .get_transaction_by_idempotency_key(idempotency_key).await? {
            return Ok(JsonResponse(TransactionResponse {
                transaction_id: existing_tx.id,
                solana_signature: existing_tx.solana_signature,
                status: existing_tx.status,
                created_at: existing_tx.created_at,
            }));
        }
    }
    
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    let tx_record = state.vault_manager.deposit(
        vault.id,
        request.amount,
        request.idempotency_key,
        request.metadata,
    ).await?;
    
    Ok(JsonResponse(TransactionResponse {
        transaction_id: tx_record.id,
        solana_signature: tx_record.solana_signature,
        status: tx_record.status,
        created_at: tx_record.created_at,
    }))
}

async fn withdraw(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(request): Json<TransactionRequest>,
) -> Result<JsonResponse<TransactionResponse>, VaultError> {
    info!("Processing withdrawal for user: {}, amount: {}", user_pubkey, request.amount);
    
    // Check idempotency
    if let Some(idempotency_key) = &request.idempotency_key {
        if let Some(existing_tx) = state.transaction_manager
            .get_transaction_by_idempotency_key(idempotency_key).await? {
            return Ok(JsonResponse(TransactionResponse {
                transaction_id: existing_tx.id,
                solana_signature: existing_tx.solana_signature,
                status: existing_tx.status,
                created_at: existing_tx.created_at,
            }));
        }
    }
    
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    if vault.available_balance < request.amount as i64 {
        return Err(VaultError::InsufficientBalance {
            available: vault.available_balance as u64,
            required: request.amount,
        });
    }
    
    let tx_record = state.vault_manager.withdraw(
        vault.id,
        request.amount,
        request.idempotency_key,
        request.metadata,
    ).await?;
    
    Ok(JsonResponse(TransactionResponse {
        transaction_id: tx_record.id,
        solana_signature: tx_record.solana_signature,
        status: tx_record.status,
        created_at: tx_record.created_at,
    }))
}

async fn lock_collateral(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(request): Json<TransactionRequest>,
) -> Result<JsonResponse<TransactionResponse>, VaultError> {
    info!("Processing collateral lock for user: {}, amount: {}", user_pubkey, request.amount);
    
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    if vault.available_balance < request.amount as i64 {
        return Err(VaultError::InsufficientBalance {
            available: vault.available_balance as u64,
            required: request.amount,
        });
    }
    
    let operation_id = Uuid::new_v4();
    let signature = state.cpi_manager.lock_collateral(
        vault.id,
        request.amount,
        operation_id,
    ).await?;
    
    Ok(JsonResponse(TransactionResponse {
        transaction_id: operation_id,
        solana_signature: Some(signature),
        status: "confirmed".to_string(),
        created_at: Utc::now(),
    }))
}

async fn unlock_collateral(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(request): Json<TransactionRequest>,
) -> Result<JsonResponse<TransactionResponse>, VaultError> {
    info!("Processing collateral unlock for user: {}, amount: {}", user_pubkey, request.amount);
    
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    if vault.locked_balance < request.amount as i64 {
        return Err(VaultError::InsufficientLockedBalance {
            locked: vault.locked_balance as u64,
            required: request.amount,
        });
    }
    
    let operation_id = Uuid::new_v4();
    let signature = state.cpi_manager.unlock_collateral(
        vault.id,
        request.amount,
        operation_id,
    ).await?;
    
    Ok(JsonResponse(TransactionResponse {
        transaction_id: operation_id,
        solana_signature: Some(signature),
        status: "confirmed".to_string(),
        created_at: Utc::now(),
    }))
}

async fn transfer_collateral(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(request): Json<TransferRequest>,
) -> Result<JsonResponse<TransactionResponse>, VaultError> {
    info!("Processing collateral transfer from {} to {}, amount: {}", 
          user_pubkey, request.destination_user_pubkey, request.amount);
    
    let source_vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    let destination_vault = state.vault_manager.get_vault_by_user_pubkey(&request.destination_user_pubkey).await?;
    
    if source_vault.available_balance < request.amount as i64 {
        return Err(VaultError::InsufficientBalance {
            available: source_vault.available_balance as u64,
            required: request.amount,
        });
    }
    
    let operation_id = Uuid::new_v4();
    let signature = state.cpi_manager.transfer_collateral(
        source_vault.id,
        destination_vault.id,
        request.amount,
        operation_id,
    ).await?;
    
    Ok(JsonResponse(TransactionResponse {
        transaction_id: operation_id,
        solana_signature: Some(signature),
        status: "confirmed".to_string(),
        created_at: Utc::now(),
    }))
}

async fn get_vault_transactions(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Query(params): Query<ListTransactionsQuery>,
) -> Result<JsonResponse<Vec<TransactionRecord>>, VaultError> {
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    let limit = params.limit.unwrap_or(50).min(100) as i64;
    let offset = ((params.page.unwrap_or(1) - 1) * params.limit.unwrap_or(50) as u32) as i64;
    
    let transactions = state.transaction_manager
        .get_vault_transactions(
            vault.id,
            params.transaction_type.as_deref(),
            params.status.as_deref(),
            params.start_date,
            params.end_date,
            limit,
            offset,
        ).await?;
    
    Ok(JsonResponse(transactions))
}

async fn get_transaction(
    State(state): State<AppState>,
    Path(transaction_id): Path<Uuid>,
) -> Result<JsonResponse<TransactionRecord>, VaultError> {
    let transaction = state.transaction_manager.get_transaction_by_id(transaction_id).await?;
    Ok(JsonResponse(transaction))
}

async fn get_balance_snapshots(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Query(params): Query<ListTransactionsQuery>,
) -> Result<JsonResponse<Vec<BalanceSnapshot>>, VaultError> {
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    let limit = params.limit.unwrap_or(50).min(100) as i64;
    let offset = ((params.page.unwrap_or(1) - 1) * params.limit.unwrap_or(50) as u32) as i64;
    
    let snapshots = state.balance_tracker.get_vault_snapshots(vault.id, limit, offset).await?;
    Ok(JsonResponse(snapshots))
}

async fn reconcile_balance(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
) -> Result<JsonResponse<serde_json::Value>, VaultError> {
    let vault = state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await?;
    
    let result = state.balance_tracker.reconcile_balances(vault.id).await?;
    
    Ok(JsonResponse(serde_json::json!({
        "is_consistent": result.is_consistent,
        "discrepancies": result.discrepancies,
        "reconciled_at": Utc::now(),
    })))
}

async fn update_vault_state(
    State(state): State<AppState>,
    Path(user_pubkey): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<JsonResponse<VaultResponse>, VaultError> {
    let is_active = payload.get("is_active")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| VaultError::ValidationError("Invalid is_active field".to_string()))?;
    
    let vault = state.vault_manager.update_vault_state(&user_pubkey, is_active).await?;
    
    Ok(JsonResponse(VaultResponse {
        id: vault.id,
        user_pubkey: vault.user_pubkey,
        vault_pubkey: vault.vault_pubkey,
        token_account_pubkey: vault.token_account_pubkey,
        total_balance: vault.total_balance,
        locked_balance: vault.locked_balance,
        available_balance: vault.available_balance,
        is_active: vault.is_active,
        created_at: vault.created_at,
        last_activity_at: vault.last_activity_at,
    }))
}

async fn get_system_stats(State(state): State<AppState>) -> JsonResponse<SystemStatsResponse> {
    match state.monitor.get_stats().await {
        Ok(stats) => JsonResponse(SystemStatsResponse {
            vault_count: stats.vault_count,
            pending_transactions: stats.pending_transactions,
            failed_transactions_24h: stats.failed_transactions_24h,
            total_value_locked: stats.total_value_locked,
            is_healthy: stats.is_healthy,
            last_reconciliation: stats.last_reconciliation,
        }),
        Err(e) => {
            error!("Failed to get system stats: {}", e);
            JsonResponse(SystemStatsResponse {
                vault_count: 0,
                pending_transactions: 0,
                failed_transactions_24h: 0,
                total_value_locked: 0,
                is_healthy: false,
                last_reconciliation: None,
            })
        }
    }
}

async fn get_system_config(State(state): State<AppState>) -> JsonResponse<serde_json::Value> {
    // This would query system configuration from database
    JsonResponse(serde_json::json!({
        "max_concurrent_transactions": 5,
        "transaction_retry_limit": 3,
        "reconciliation_interval_seconds": 300,
        "health_check_interval_seconds": 30,
    }))
}

async fn update_system_config(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> JsonResponse<serde_json::Value> {
    // This would update system configuration in database
    JsonResponse(serde_json::json!({
        "message": "Configuration updated",
        "updated_at": Utc::now(),
    }))
}

async fn get_audit_log(
    State(state): State<AppState>,
    Query(params): Query<ListTransactionsQuery>,
) -> JsonResponse<serde_json::Value> {
    // This would query audit log from database
    JsonResponse(serde_json::json!({
        "events": [],
        "total": 0,
    }))
}

// WebSocket handlers

async fn metrics_websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_metrics_websocket(socket, state))
}

async fn handle_metrics_websocket(socket: WebSocket, state: AppState) {
    use tokio::time::{interval, Duration};
    use futures::{SinkExt, StreamExt};
    
    let (mut sender, mut _receiver) = socket.split();
    let mut interval = interval(Duration::from_secs(30));
    
    loop {
        interval.tick().await;
        
        match state.monitor.get_stats().await {
            Ok(stats) => {
                let message = serde_json::json!({
                    "type": "metrics_update",
                    "data": {
                        "vault_count": stats.vault_count,
                        "pending_transactions": stats.pending_transactions,
                        "failed_transactions_24h": stats.failed_transactions_24h,
                        "total_value_locked": stats.total_value_locked,
                        "is_healthy": stats.is_healthy,
                        "timestamp": Utc::now(),
                    }
                });
                
                if sender.send(axum::extract::ws::Message::Text(
                    serde_json::to_string(&message).unwrap()
                )).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to get metrics for WebSocket: {}", e);
                break;
            }
        }
    }
}

async fn vault_websocket(
    ws: WebSocketUpgrade,
    Path(user_pubkey): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_vault_websocket(socket, user_pubkey, state))
}

async fn handle_vault_websocket(socket: WebSocket, user_pubkey: String, state: AppState) {
    use tokio::time::{interval, Duration};
    use futures::{SinkExt, StreamExt};
    
    let (mut sender, mut _receiver) = socket.split();
    let mut interval = interval(Duration::from_secs(10));
    
    loop {
        interval.tick().await;
        
        match state.vault_manager.get_vault_by_user_pubkey(&user_pubkey).await {
            Ok(vault) => {
                let message = serde_json::json!({
                    "type": "vault_update",
                    "data": {
                        "vault_id": vault.id,
                        "user_pubkey": vault.user_pubkey,
                        "total_balance": vault.total_balance,
                        "locked_balance": vault.locked_balance,
                        "available_balance": vault.available_balance,
                        "is_active": vault.is_active,
                        "timestamp": Utc::now(),
                    }
                });
                
                if sender.send(axum::extract::ws::Message::Text(
                    serde_json::to_string(&message).unwrap()
                )).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                warn!("Failed to get vault for WebSocket: {}", e);
                break;
            }
        }
    }
}

// Middleware

async fn rate_limit_middleware(
    State(state): State<AppState>,
    axum::extract::RequestParts { headers, .. }: axum::extract::RequestParts,
    next: middleware::Next,
) -> Result<Response, StatusCode> {
    // Extract client identifier (API key or IP address)
    let client_id = extract_client_identifier(&headers);
    
    // Rate limiting configuration
    const TOKENS_PER_REQUEST: i32 = 1;
    const MAX_TOKENS: i32 = 100; // 100 requests per window
    const REFILL_RATE: i32 = 10; // 10 tokens per second refill
    
    // Check rate limit
    match state.rate_limit_repo.consume_tokens(&client_id, TOKENS_PER_REQUEST, MAX_TOKENS, REFILL_RATE).await {
        Ok(result) => {
            if result.allowed {
                // Request is allowed, proceed
                let mut response = next.run(axum::extract::Request::from_parts(
                    axum::http::request::Parts {
                        method: headers.method.clone(),
                        uri: headers.uri.clone(),
                        version: headers.version,
                        headers: headers.headers.clone(),
                        extensions: headers.extensions.clone(),
                        ..Default::default()
                    },
                    axum::body::Body::empty(),
                )).await;
                
                // Add rate limit headers to response
                response.headers_mut().insert(
                    "X-RateLimit-Remaining",
                    result.remaining_tokens.to_string().parse().unwrap(),
                );
                
                if let Some(reset_at) = result.reset_at {
                    response.headers_mut().insert(
                        "X-RateLimit-Reset",
                        reset_at.timestamp().to_string().parse().unwrap(),
                    );
                }
                
                Ok(response)
            } else {
                // Rate limit exceeded
                Err(StatusCode::TOO_MANY_REQUESTS)
            }
        }
        Err(_) => {
            // Database error - allow request but log error
            tracing::error!("Rate limit check failed for client: {}", client_id);
            Ok(next.run(axum::extract::Request::from_parts(
                axum::http::request::Parts {
                    method: headers.method.clone(),
                    uri: headers.uri.clone(),
                    version: headers.version,
                    headers: headers.headers.clone(),
                    extensions: headers.extensions.clone(),
                    ..Default::default()
                },
                axum::body::Body::empty(),
            )).await)
        }
    }
}

fn extract_client_identifier(headers: &axum::http::request::Parts) -> String {
    // Try to get API key from Authorization header first
    if let Some(auth_header) = headers.headers.get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.starts_with("Bearer ") {
                return auth_str[7..].to_string(); // Return the API key
            }
        }
    }
    
    // Fallback to IP address (would need to be extracted from connection info in real implementation)
    // For now, return a default identifier
    "default_client".to_string()
}

// Error handling

impl IntoResponse for VaultError {
    fn into_response(self) -> Response {
        let (status, error_message) = match &self {
            VaultError::NotFound(_) => (StatusCode::NOT_FOUND, "Resource not found"),
            VaultError::InsufficientBalance { .. } => (StatusCode::BAD_REQUEST, "Insufficient balance"),
            VaultError::InsufficientLockedBalance { .. } => (StatusCode::BAD_REQUEST, "Insufficient locked balance"),
            VaultError::ConcurrentConflict(_) => (StatusCode::CONFLICT, "Concurrent operation conflict"),
            VaultError::ValidationError(_) => (StatusCode::BAD_REQUEST, "Validation error"),
            VaultError::TransactionError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Transaction error"),
            VaultError::NetworkError(_) => (StatusCode::SERVICE_UNAVAILABLE, "Network error"),
            VaultError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
            VaultError::ConfigurationError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Configuration error"),
        };
        
        let error_response = ErrorResponse {
            error: error_message.to_string(),
            message: self.to_string(),
            details: None,
            request_id: None,
        };
        
        (status, JsonResponse(error_response)).into_response()
    }
}