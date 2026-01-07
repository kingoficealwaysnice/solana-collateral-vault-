use crate::error::{Result, VaultError};
use crate::models::{Vault, BalanceSnapshot, SystemBalanceStats};
use crate::vault_manager::VaultManager;
use crate::balance_tracker::BalanceTracker;
use crate::transaction_builder::{TransactionBuilder, TransactionSubmitter};
use crate::database::{VaultRepository, TransactionRepository, SnapshotRepository, AuditRepository};
use chrono::{DateTime, Utc, Duration};
use std::sync::Arc;
use tokio::time::interval;
use tracing::{info, warn, error};
use uuid::Uuid;

/// VaultMonitor provides real-time monitoring and alerting for the vault system
pub struct VaultMonitor {
    pool: sqlx::PgPool,
    vault_repo: VaultRepository,
    transaction_repo: TransactionRepository,
    snapshot_repo: SnapshotRepository,
    audit_repo: AuditRepository,
    vault_manager: Arc<VaultManager>,
    balance_tracker: Arc<BalanceTracker>,
    transaction_builder: Arc<TransactionBuilder>,
    transaction_submitter: Arc<TransactionSubmitter>,
    
    // Configuration
    reconciliation_interval_seconds: u64,
    health_check_interval_seconds: u64,
    stale_transaction_threshold_seconds: i64,
    max_pending_transactions: i64,
    
    // Monitoring state
    last_reconciliation: Option<DateTime<Utc>>,
    consecutive_failures: u32,
    is_healthy: Arc<tokio::sync::RwLock<bool>>,
}

impl VaultMonitor {
    pub fn new(
        pool: sqlx::PgPool,
        vault_manager: Arc<VaultManager>,
        balance_tracker: Arc<BalanceTracker>,
        transaction_builder: Arc<TransactionBuilder>,
        transaction_submitter: Arc<TransactionSubmitter>,
        config: MonitorConfig,
    ) -> Self {
        Self {
            pool: pool.clone(),
            vault_repo: VaultRepository::new(pool.clone()),
            transaction_repo: TransactionRepository::new(pool.clone()),
            snapshot_repo: SnapshotRepository::new(pool.clone()),
            audit_repo: AuditRepository::new(pool),
            vault_manager,
            balance_tracker,
            transaction_builder,
            transaction_submitter,
            reconciliation_interval_seconds: config.reconciliation_interval_seconds,
            health_check_interval_seconds: config.health_check_interval_seconds,
            stale_transaction_threshold_seconds: config.stale_transaction_threshold_seconds,
            max_pending_transactions: config.max_pending_transactions,
            last_reconciliation: None,
            consecutive_failures: 0,
            is_healthy: Arc::new(tokio::sync::RwLock::new(true)),
        }
    }
    
    /// Start monitoring tasks
    pub async fn start_monitoring(&self) {
        info!("Starting vault monitoring services");
        
        // Start reconciliation task
        let reconciliation_handle = self.start_reconciliation_task();
        
        // Start health check task
        let health_check_handle = self.start_health_check_task();
        
        // Start stale transaction cleanup task
        let cleanup_handle = self.start_cleanup_task();
        
        // Start balance snapshot task
        let snapshot_handle = self.start_snapshot_task();
        
        // Wait for all tasks
        tokio::select! {
            _ = reconciliation_handle => warn!("Reconciliation task ended"),
            _ = health_check_handle => warn!("Health check task ended"),
            _ = cleanup_handle => warn!("Cleanup task ended"),
            _ = snapshot_handle => warn!("Snapshot task ended"),
        }
    }
    
    /// Start reconciliation task
    fn start_reconciliation_task(&self) -> tokio::task::JoinHandle<()> {
        let monitor = Arc::new(self);
        let mut interval = interval(tokio::time::Duration::from_secs(self.reconciliation_interval_seconds));
        
        tokio::spawn(async move {
            loop {
                interval.tick().await;
                
                if let Err(e) = monitor.run_reconciliation().await {
                    error!("Reconciliation failed: {}", e);
                    monitor.increment_failures().await;
                } else {
                    monitor.reset_failures().await;
                }
            }
        })
    }
    
    /// Start health check task
    fn start_health_check_task(&self) -> tokio::task::JoinHandle<()> {
        let monitor = Arc::new(self);
        let mut interval = interval(tokio::time::Duration::from_secs(self.health_check_interval_seconds));
        
        tokio::spawn(async move {
            loop {
                interval.tick().await;
                
                match monitor.run_health_check().await {
                    Ok(healthy) => {
                        monitor.set_health_status(healthy).await;
                        if !healthy {
                            warn!("System health check failed");
                        }
                    }
                    Err(e) => {
                        error!("Health check error: {}", e);
                        monitor.set_health_status(false).await;
                    }
                }
            }
        })
    }
    
    /// Start cleanup task
    fn start_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        let monitor = Arc::new(self);
        let mut interval = interval(tokio::time::Duration::from_secs(300)); // Every 5 minutes
        
        tokio::spawn(async move {
            loop {
                interval.tick().await;
                
                if let Err(e) = monitor.cleanup_stale_transactions().await {
                    error!("Cleanup failed: {}", e);
                }
            }
        })
    }
    
    /// Start snapshot task
    fn start_snapshot_task(&self) -> tokio::task::JoinHandle<()> {
        let monitor = Arc::new(self);
        let mut interval = interval(tokio::time::Duration::from_secs(60)); // Every minute
        
        tokio::spawn(async move {
            loop {
                interval.tick().await;
                
                if let Err(e) = monitor.create_balance_snapshots().await {
                    error!("Snapshot creation failed: {}", e);
                }
            }
        })
    }
    
    /// Run balance reconciliation
    async fn run_reconciliation(&self) -> Result<()> {
        info!("Running balance reconciliation");
        
        // Get all active vaults
        let vaults = self.vault_repo.get_active_vaults(1000, 0).await?;
        
        let mut inconsistent_vaults = Vec::new();
        let mut total_discrepancies = 0;
        
        for vault in vaults {
            match self.balance_tracker.reconcile_balances(vault.id).await {
                Ok(result) => {
                    if !result.is_consistent {
                        inconsistent_vaults.push((vault.id, result.discrepancies.len()));
                        total_discrepancies += result.discrepancies.len();
                        
                        // Log critical discrepancies
                        for discrepancy in &result.discrepancies {
                            if discrepancy.severity == crate::balance_tracker::DiscrepancySeverity::Critical {
                                error!("CRITICAL: Vault {} has critical discrepancy: {}", 
                                       vault.id, discrepancy.issue);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to reconcile vault {}: {}", vault.id, e);
                    inconsistent_vaults.push((vault.id, 1));
                    total_discrepancies += 1;
                }
            }
        }
        
        if !inconsistent_vaults.is_empty() {
            warn!("Balance reconciliation found {} inconsistent vaults with {} total discrepancies",
                  inconsistent_vaults.len(), total_discrepancies);
        } else {
            info!("Balance reconciliation completed successfully - all vaults consistent");
        }
        
        self.last_reconciliation = Some(Utc::now());
        Ok(())
    }
    
    /// Run health check
    async fn run_health_check(&self) -> Result<bool> {
        // Check database connection
        let db_healthy = self.check_database_connection().await?;
        
        // Check Solana connection
        let solana_healthy = self.check_solana_connection().await?;
        
        // Check pending transactions
        let pending_healthy = self.check_pending_transactions().await?;
        
        // Check for critical issues
        let critical_issues = self.check_critical_issues().await?;
        
        let overall_healthy = db_healthy && solana_healthy && pending_healthy && !critical_issues;
        
        if !overall_healthy {
            warn!("Health check failed: db={}, solana={}, pending={}, critical={}", 
                  db_healthy, solana_healthy, pending_healthy, critical_issues);
        }
        
        Ok(overall_healthy)
    }
    
    /// Check database connection
    async fn check_database_connection(&self) -> Result<bool> {
        match sqlx::query("SELECT 1").fetch_one(&self.pool).await {
            Ok(_) => Ok(true),
            Err(e) => {
                error!("Database connection check failed: {}", e);
                Ok(false)
            }
        }
    }
    
    /// Check Solana connection
    async fn check_solana_connection(&self) -> Result<bool> {
        match self.transaction_builder.rpc_client.get_health() {
            Ok(_) => Ok(true),
            Err(e) => {
                error!("Solana connection check failed: {}", e);
                Ok(false)
            }
        }
    }
    
    /// Check pending transactions
    async fn check_pending_transactions(&self) -> Result<bool> {
        let pending_count = self.transaction_repo.get_pending_transactions_count().await?;
        let healthy = pending_count < self.max_pending_transactions;
        
        if !healthy {
            warn!("Too many pending transactions: {} (threshold: {})", pending_count, self.max_pending_transactions);
        }
        
        Ok(healthy)
    }
    
    /// Check for critical issues
    async fn check_critical_issues(&self) -> Result<bool> {
        // Check for vaults with negative balances
        let negative_balances = sqlx::query!(
            "SELECT COUNT(*) as count FROM vaults WHERE total_balance < 0 OR locked_balance < 0 OR available_balance < 0",
        )
        .fetch_one(&self.pool)
        .await?;
        
        if negative_balances.count.unwrap_or(0) > 0 {
            error!("Found {} vaults with negative balances", negative_balances.count.unwrap_or(0));
            return Ok(true); // Critical issue found
        }
        
        // Check for balance invariant violations
        let invariant_violations = sqlx::query!(
            "SELECT COUNT(*) as count FROM vaults WHERE total_balance != (locked_balance + available_balance)",
        )
        .fetch_one(&self.pool)
        .await?;
        
        if invariant_violations.count.unwrap_or(0) > 0 {
            error!("Found {} vaults with balance invariant violations", invariant_violations.count.unwrap_or(0));
            return Ok(true); // Critical issue found
        }
        
        Ok(false) // No critical issues
    }
    
    /// Cleanup stale transactions
    async fn cleanup_stale_transactions(&self) -> Result<()> {
        let cutoff_time = Utc::now() - Duration::seconds(self.stale_transaction_threshold_seconds);
        
        let cleaned_count = self.transaction_repo.cleanup_stale_transactions(cutoff_time).await?;
        
        if cleaned_count > 0 {
            info!("Cleaned up {} stale transactions", cleaned_count);
        }
        
        Ok(())
    }
    
    /// Create balance snapshots for all active vaults
    async fn create_balance_snapshots(&self) -> Result<()> {
        // Get current block height
        let block_height = match self.transaction_builder.rpc_client.get_block_height() {
            Ok(height) => Some(height as i64),
            Err(e) => {
                warn!("Failed to get block height: {}", e);
                None
            }
        };
        
        // Get all active vaults
        let vaults = self.vault_repo.get_active_vaults(1000, 0).await?;
        
        for vault in vaults {
            if let Err(e) = self.balance_tracker.create_snapshot(vault.id, block_height).await {
                error!("Failed to create snapshot for vault {}: {}", vault.id, e);
            }
        }
        
        Ok(())
    }
    
    /// Get system health status
    pub async fn get_health_status(&self) -> bool {
        *self.is_healthy.read().await
    }
    
    /// Get monitoring statistics
    pub async fn get_stats(&self) -> Result<MonitoringStats> {
        let system_stats = self.balance_tracker.get_system_stats().await?;
        let pending_count = self.transaction_repo.get_pending_transactions_count().await?;
        
        // Get failed transactions in last 24h
        let failed_tx_count = sqlx::query!(
            "SELECT COUNT(*) as count FROM transaction_records WHERE status = 'failed' AND created_at > $1",
            Utc::now() - Duration::hours(24)
        )
        .fetch_one(&self.pool)
        .await?;
        
        Ok(MonitoringStats {
            vault_count: system_stats.vault_count,
            pending_transactions: pending_count,
            failed_transactions_24h: failed_tx_count.count.unwrap_or(0),
            total_value_locked: system_stats.total_value_locked,
            is_healthy: self.get_health_status().await,
            last_reconciliation: self.last_reconciliation,
            consecutive_failures: self.consecutive_failures,
        })
    }
    
    /// Set health status
    async fn set_health_status(&self, healthy: bool) {
        let mut status = self.is_healthy.write().await;
        *status = healthy;
    }
    
    /// Increment failure counter
    async fn increment_failures(&self) {
        // For now, we'll use atomic operations for simple counting
        // In a production system, you might want to persist this to the database
        use std::sync::atomic::{AtomicU32, Ordering};
        static FAILURE_COUNT: AtomicU32 = AtomicU32::new(0);
        FAILURE_COUNT.fetch_add(1, Ordering::SeqCst);
    }
    
    /// Reset failure counter
    async fn reset_failures(&self) {
        // Reset the atomic counter
        use std::sync::atomic::{AtomicU32, Ordering};
        static FAILURE_COUNT: AtomicU32 = AtomicU32::new(0);
        FAILURE_COUNT.store(0, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub reconciliation_interval_seconds: u64,
    pub health_check_interval_seconds: u64,
    pub stale_transaction_threshold_seconds: i64,
    pub max_pending_transactions: i64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            reconciliation_interval_seconds: 300, // 5 minutes
            health_check_interval_seconds: 30,    // 30 seconds
            stale_transaction_threshold_seconds: 3600, // 1 hour
            max_pending_transactions: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MonitoringStats {
    pub vault_count: i64,
    pub pending_transactions: i64,
    pub failed_transactions_24h: i64,
    pub total_value_locked: i64,
    pub is_healthy: bool,
    pub last_reconciliation: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}