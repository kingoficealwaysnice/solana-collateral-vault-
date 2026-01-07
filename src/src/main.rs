use collateral_vault_backend::{
    VaultManager, TransactionManager, BalanceTracker, TransactionBuilder, TransactionSubmitter,
    CPIManager, VaultMonitor, MonitorConfig, models::*, error::Result, database::RateLimitRepository,
    api,
};
use sqlx::postgres::PgPoolOptions;
use solana_client::rpc_client::RpcClient;
use solana_sdk::signature::{Keypair, Signer};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, error};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    
    info!("Starting Collateral Vault Backend Service");
    
    // Load configuration
    let config = load_config()?;
    
    // Initialize database connection pool
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&config.database_url)
        .await?;
    
    info!("Database connection established");
    
    // Run database migrations
    sqlx::migrate!("./migrations").run(&pool).await?;
    info!("Database migrations completed");
    
    // Initialize Solana RPC client
    let rpc_client = Arc::new(RpcClient::new(config.solana_rpc_url.clone()));
    info!("Solana RPC client initialized: {}", config.solana_rpc_url);
    
    // Load payer keypair
    let payer_keypair = load_payer_keypair(&config.payer_keypair_path)?;
    info!("Payer keypair loaded: {}", payer_keypair.pubkey());
    
    // Initialize core services
    let vault_manager = Arc::new(VaultManager::new(pool.clone()));
    let transaction_manager = Arc::new(TransactionManager::new(pool.clone()));
    let balance_tracker = Arc::new(BalanceTracker::new(pool.clone(), config.reconciliation_window_seconds));
    
    let transaction_builder = Arc::new(TransactionBuilder::new(
        &config.solana_rpc_url,
        payer_keypair,
        config.program_id.parse()?,
        config.max_concurrent_transactions,
    )?);
    
    let transaction_submitter = Arc::new(TransactionSubmitter::new(
        rpc_client.clone(),
        config.max_transaction_retries,
        config.retry_delay_ms,
    ));
    
    // Initialize CPI manager for trading operations
    let authority_keypair = Arc::new(load_authority_keypair(&config.authority_keypair_path)?);
    let cpi_manager = Arc::new(CPIManager::new(
        vault_manager.clone(),
        transaction_builder.clone(),
        transaction_submitter.clone(),
        authority_keypair,
    ));
    
    // Initialize monitoring service
    let monitor_config = MonitorConfig {
        reconciliation_interval_seconds: config.reconciliation_interval_seconds as u64,
        health_check_interval_seconds: config.health_check_interval_seconds as u64,
        stale_transaction_threshold_seconds: config.stale_transaction_threshold_seconds,
        max_pending_transactions: config.max_pending_transactions,
    };
    
    let monitor = Arc::new(VaultMonitor::new(
        pool.clone(),
        vault_manager.clone(),
        balance_tracker.clone(),
        transaction_builder.clone(),
        transaction_submitter.clone(),
        monitor_config,
    ));
    
    // Start monitoring in background
    let monitor_handle = {
        let monitor = monitor.clone();
        tokio::spawn(async move {
            monitor.start_monitoring().await;
        })
    };
    
    info!("All services initialized successfully");
    
    // Start API server
    start_api_server(
        vault_manager,
        transaction_manager,
        balance_tracker,
        cpi_manager,
        monitor,
        pool,
        config.api_port,
    ).await?;
    
    // Wait for monitor task
    monitor_handle.await?;
    
    Ok(())
}

#[derive(Debug, Clone)]
struct Config {
    database_url: String,
    database_max_connections: u32,
    solana_rpc_url: String,
    payer_keypair_path: String,
    authority_keypair_path: String,
    program_id: String,
    max_concurrent_transactions: usize,
    max_transaction_retries: u32,
    retry_delay_ms: u64,
    reconciliation_window_seconds: i64,
    reconciliation_interval_seconds: u64,
    health_check_interval_seconds: u64,
    stale_transaction_threshold_seconds: i64,
    max_pending_transactions: i64,
    api_port: u16,
}

fn load_config() -> Result<Config> {
    dotenv::dotenv().ok();
    
    Ok(Config {
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://user:password@localhost/vault_db".to_string()),
        database_max_connections: std::env::var("DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid DATABASE_MAX_CONNECTIONS".to_string()))?,
        solana_rpc_url: std::env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
        payer_keypair_path: std::env::var("PAYER_KEYPAIR_PATH")
            .unwrap_or_else(|_| "./keys/payer.json".to_string()),
        authority_keypair_path: std::env::var("AUTHORITY_KEYPAIR_PATH")
            .unwrap_or_else(|_| "./keys/authority.json".to_string()),
        program_id: std::env::var("PROGRAM_ID")
            .unwrap_or_else(|_| "CVault111111111111111111111111111111111111111".to_string()),
        max_concurrent_transactions: std::env::var("MAX_CONCURRENT_TRANSACTIONS")
            .unwrap_or_else(|_| "5".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid MAX_CONCURRENT_TRANSACTIONS".to_string()))?,
        max_transaction_retries: std::env::var("MAX_TRANSACTION_RETRIES")
            .unwrap_or_else(|_| "3".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid MAX_TRANSACTION_RETRIES".to_string()))?,
        retry_delay_ms: std::env::var("RETRY_DELAY_MS")
            .unwrap_or_else(|_| "1000".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid RETRY_DELAY_MS".to_string()))?,
        reconciliation_window_seconds: std::env::var("RECONCILIATION_WINDOW_SECONDS")
            .unwrap_or_else(|_| "3600".to_string()) // 1 hour
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid RECONCILIATION_WINDOW_SECONDS".to_string()))?,
        reconciliation_interval_seconds: std::env::var("RECONCILIATION_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "300".to_string()) // 5 minutes
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid RECONCILIATION_INTERVAL_SECONDS".to_string()))?,
        health_check_interval_seconds: std::env::var("HEALTH_CHECK_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid HEALTH_CHECK_INTERVAL_SECONDS".to_string()))?,
        stale_transaction_threshold_seconds: std::env::var("STALE_TRANSACTION_THRESHOLD_SECONDS")
            .unwrap_or_else(|_| "3600".to_string()) // 1 hour
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid STALE_TRANSACTION_THRESHOLD_SECONDS".to_string()))?,
        max_pending_transactions: std::env::var("MAX_PENDING_TRANSACTIONS")
            .unwrap_or_else(|_| "100".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid MAX_PENDING_TRANSACTIONS".to_string()))?,
        api_port: std::env::var("API_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .map_err(|_| collateral_vault_backend::VaultError::ConfigurationError("Invalid API_PORT".to_string()))?,
    })
}

fn load_payer_keypair(path: &str) -> Result<Keypair> {
    let keypair_data = std::fs::read_to_string(path)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Failed to read payer keypair: {}", e)))?;
    
    let keypair_bytes: Vec<u8> = serde_json::from_str(&keypair_data)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Invalid payer keypair JSON: {}", e)))?;
    
    Keypair::from_bytes(&keypair_bytes)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Invalid payer keypair: {}", e)))
}

fn load_authority_keypair(path: &str) -> Result<Keypair> {
    let keypair_data = std::fs::read_to_string(path)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Failed to read authority keypair: {}", e)))?;
    
    let keypair_bytes: Vec<u8> = serde_json::from_str(&keypair_data)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Invalid authority keypair JSON: {}", e)))?;
    
    Keypair::from_bytes(&keypair_bytes)
        .map_err(|e| collateral_vault_backend::VaultError::ConfigurationError(format!("Invalid authority keypair: {}", e)))
}

async fn start_api_server(
    vault_manager: Arc<VaultManager>,
    transaction_manager: Arc<TransactionManager>,
    balance_tracker: Arc<BalanceTracker>,
    cpi_manager: Arc<CPIManager>,
    monitor: Arc<VaultMonitor>,
    pool: sqlx::PgPool,
    port: u16,
) -> Result<()> {
    use std::net::SocketAddr;
    
    // Create rate limit repository
    let rate_limit_repo = Arc::new(RateLimitRepository::new(pool));
    
    // Create app state using the proper api::AppState
    let app_state = api::AppState {
        vault_manager,
        transaction_manager,
        balance_tracker,
        cpi_manager,
        monitor,
        rate_limit_repo,
    };
    
    // Create router using the api module
    let app = api::create_router(app_state);
    
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("API server listening on {}", addr);
    
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .map_err(|e| collateral_vault_backend::VaultError::NetworkError(format!("API server error: {}", e)))?;
    
    Ok(())
}

// API handlers would be implemented here
// For brevity, I'm showing the structure but not full implementations

async fn health_check(State(state): State<AppState>) -> JsonResponse<serde_json::Value> {
    let is_healthy = state.monitor.get_health_status().await;
    JsonResponse(serde_json::json!({
        "status": if is_healthy { "healthy" } else { "unhealthy" },
        "timestamp": chrono::Utc::now(),
    }))
}

async fn list_vaults(State(state): State<AppState>) -> JsonResponse<serde_json::Value> {
    // Implementation would query vault_manager and return paginated results
    JsonResponse(serde_json::json!({"vaults": []}))
}

async fn create_vault(State(state): State<AppState>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would create vault through vault_manager
    JsonResponse(serde_json::json!({"message": "Vault created"}))
}

async fn get_vault(State(state): State<AppState>, Path(user_pubkey): Path<String>) -> JsonResponse<serde_json::Value> {
    // Implementation would get vault by user pubkey
    JsonResponse(serde_json::json!({"vault": null}))
}

async fn get_balance(State(state): State<AppState>, Path(user_pubkey): Path<String>) -> JsonResponse<serde_json::Value> {
    // Implementation would get balance through balance_tracker
    JsonResponse(serde_json::json!({"balance": null}))
}

async fn deposit(State(state): State<AppState>, Path(user_pubkey): Path<String>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would process deposit
    JsonResponse(serde_json::json!({"message": "Deposit processed"}))
}

async fn withdraw(State(state): State<AppState>, Path(user_pubkey): Path<String>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would process withdrawal
    JsonResponse(serde_json::json!({"message": "Withdrawal processed"}))
}

async fn lock_collateral(State(state): State<AppState>, Path(user_pubkey): Path<String>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would lock collateral through cpi_manager
    JsonResponse(serde_json::json!({"message": "Collateral locked"}))
}

async fn unlock_collateral(State(state): State<AppState>, Path(user_pubkey): Path<String>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would unlock collateral through cpi_manager
    JsonResponse(serde_json::json!({"message": "Collateral unlocked"}))
}

async fn transfer_collateral(State(state): State<AppState>, Path(user_pubkey): Path<String>, Json(payload): Json<serde_json::Value>) -> JsonResponse<serde_json::Value> {
    // Implementation would transfer collateral through cpi_manager
    JsonResponse(serde_json::json!({"message": "Collateral transferred"}))
}

async fn get_vault_transactions(State(state): State<AppState>, Path(vault_id): Path<uuid::Uuid>) -> JsonResponse<serde_json::Value> {
    // Implementation would get transactions through transaction_manager
    JsonResponse(serde_json::json!({"transactions": []}))
}

async fn get_system_stats(State(state): State<AppState>) -> JsonResponse<serde_json::Value> {
    // Implementation would get stats through monitor
    JsonResponse(serde_json::json!({"stats": null}))
}