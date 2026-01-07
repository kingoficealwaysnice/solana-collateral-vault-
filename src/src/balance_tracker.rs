use crate::error::{Result, VaultError};
use crate::models::{Vault, BalanceSnapshot, SystemBalanceStats};
use crate::database::{VaultRepository, SnapshotRepository};
use chrono::{DateTime, Utc, Duration};
use std::collections::HashMap;
use tokio::sync::RwLock;
use std::sync::Arc;
use uuid::Uuid;
use tracing::{info, warn, error};

/// Balance tracker for real-time balance monitoring and reconciliation
pub struct BalanceTracker {
    vault_repo: VaultRepository,
    snapshot_repo: SnapshotRepository,
    cache: Arc<RwLock<HashMap<Uuid, BalanceCache>>>,
    reconciliation_window: Duration,
}

#[derive(Debug, Clone)]
struct BalanceCache {
    total_balance: u64,
    locked_balance: u64,
    available_balance: u64,
    last_updated: DateTime<Utc>,
    last_snapshot: Option<DateTime<Utc>>,
}

impl BalanceTracker {
    pub fn new(pool: sqlx::PgPool, reconciliation_window_seconds: i64) -> Self {
        Self {
            vault_repo: VaultRepository::new(pool.clone()),
            snapshot_repo: SnapshotRepository::new(pool),
            cache: Arc::new(RwLock::new(HashMap::new())),
            reconciliation_window: Duration::seconds(reconciliation_window_seconds),
        }
    }
    
    /// Get current balance for a vault (from cache or database)
    pub async fn get_balance(&self, vault_id: Uuid) -> Result<(u64, u64, u64)> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get(&vault_id) {
                // Return cached data if it's recent (within 5 seconds)
                if Utc::now() - cached.last_updated < Duration::seconds(5) {
                    return Ok((cached.total_balance, cached.locked_balance, cached.available_balance));
                }
            }
        }
        
        // Fetch from database
        let vault = self.vault_repo.get_vault_by_id(vault_id).await?;
        
        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(vault_id, BalanceCache {
                total_balance: vault.total_balance as u64,
                locked_balance: vault.locked_balance as u64,
                available_balance: vault.available_balance as u64,
                last_updated: Utc::now(),
                last_snapshot: None,
            });
        }
        
        Ok((vault.total_balance as u64, vault.locked_balance as u64, vault.available_balance as u64))
    }
    
    /// Update cached balance for a vault
    pub async fn update_cached_balance(&self, vault_id: Uuid, total: u64, locked: u64, available: u64) {
        let mut cache = self.cache.write().await;
        cache.insert(vault_id, BalanceCache {
            total_balance: total,
            locked_balance: locked,
            available_balance: available,
            last_updated: Utc::now(),
            last_snapshot: None,
        });
    }
    
    /// Create balance snapshot for reconciliation
    pub async fn create_snapshot(&self, vault_id: Uuid, block_height: Option<i64>) -> Result<BalanceSnapshot> {
        let (total, locked, available) = self.get_balance(vault_id).await?;
        
        let snapshot = self.snapshot_repo.create_snapshot(
            vault_id,
            total as i64,
            locked as i64,
            available as i64,
            block_height
        ).await?;
        
        // Update cache with snapshot time
        {
            let mut cache = self.cache.write().await;
            if let Some(cached) = cache.get_mut(&vault_id) {
                cached.last_snapshot = Some(snapshot.created_at);
            }
        }
        
        info!("Created balance snapshot for vault {} at block {:?}", vault_id, block_height);
        Ok(snapshot)
    }
    
    /// Reconcile balances between on-chain and database state
    pub async fn reconcile_balances(&self, vault_id: Uuid) -> Result<ReconciliationResult> {
        let vault = self.vault_repo.get_vault_by_id(vault_id).await?;
        let (cached_total, cached_locked, cached_available) = self.get_balance(vault_id).await?;
        
        let mut discrepancies = Vec::new();
        
        // Check total balance
        if vault.total_balance != cached_total as i64 {
            discrepancies.push(Discrepancy {
                field: "total_balance".to_string(),
                database_value: vault.total_balance,
                cached_value: cached_total as i64,
                severity: DiscrepancySeverity::High,
                issue: format!("Total balance mismatch: DB={}, Cache={}", vault.total_balance, cached_total),
            });
        }
        
        // Check locked balance
        if vault.locked_balance != cached_locked as i64 {
            discrepancies.push(Discrepancy {
                field: "locked_balance".to_string(),
                database_value: vault.locked_balance,
                cached_value: cached_locked as i64,
                severity: DiscrepancySeverity::High,
                issue: format!("Locked balance mismatch: DB={}, Cache={}", vault.locked_balance, cached_locked),
            });
        }
        
        // Check available balance
        if vault.available_balance != cached_available as i64 {
            discrepancies.push(Discrepancy {
                field: "available_balance".to_string(),
                database_value: vault.available_balance,
                cached_value: cached_available as i64,
                severity: DiscrepancySeverity::High,
                issue: format!("Available balance mismatch: DB={}, Cache={}", vault.available_balance, cached_available),
            });
        }
        
        // Check balance invariant
        if vault.total_balance != vault.locked_balance + vault.available_balance {
            discrepancies.push(Discrepancy {
                field: "balance_invariant".to_string(),
                database_value: vault.total_balance,
                cached_value: (vault.locked_balance + vault.available_balance),
                severity: DiscrepancySeverity::Critical,
                issue: format!("Balance invariant violated: total={} != locked={} + available={}", 
                             vault.total_balance, vault.locked_balance, vault.available_balance),
            });
        }
        
        let is_consistent = discrepancies.is_empty();
        
        if !is_consistent {
            warn!("Balance reconciliation found {} discrepancies for vault {}", discrepancies.len(), vault_id);
            for discrepancy in &discrepancies {
                match discrepancy.severity {
                    DiscrepancySeverity::Critical => error!("CRITICAL: {}", discrepancy.issue),
                    DiscrepancySeverity::High => warn!("HIGH: {}", discrepancy.issue),
                    DiscrepancySeverity::Medium => info!("MEDIUM: {}", discrepancy.issue),
                }
            }
        }
        
        Ok(ReconciliationResult {
            vault_id,
            is_consistent,
            discrepancies,
            database_total: vault.total_balance,
            cached_total: cached_total as i64,
            last_reconciliation: Utc::now(),
        })
    }
    
    /// Get system-wide balance statistics
    pub async fn get_system_stats(&self) -> Result<SystemBalanceStats> {
        self.snapshot_repo.get_system_stats().await
    }
    
    /// Get recent snapshots for a vault
    pub async fn get_vault_snapshots(&self, vault_id: Uuid, limit: i32) -> Result<Vec<BalanceSnapshot>> {
        self.snapshot_repo.get_vault_snapshots(vault_id, limit).await
    }
    
    /// Check if vault needs reconciliation (based on time window)
    pub async fn needs_reconciliation(&self, vault_id: Uuid) -> Result<bool> {
        let snapshots = self.snapshot_repo.get_vault_snapshots(vault_id, 1).await?;
        
        if snapshots.is_empty() {
            return Ok(true); // No snapshots, needs reconciliation
        }
        
        let last_snapshot = &snapshots[0];
        let time_since_last = Utc::now() - last_snapshot.created_at;
        
        Ok(time_since_last > self.reconciliation_window)
    }
    
    /// Bulk reconciliation for multiple vaults
    pub async fn bulk_reconcile(&self, vault_ids: Vec<Uuid>) -> Result<BulkReconciliationResult> {
        let mut results = Vec::new();
        let mut total_inconsistent = 0;
        let mut total_discrepancies = 0;
        
        for vault_id in vault_ids {
            match self.reconcile_balances(vault_id).await {
                Ok(result) => {
                    if !result.is_consistent {
                        total_inconsistent += 1;
                        total_discrepancies += result.discrepancies.len();
                    }
                    results.push(result);
                }
                Err(e) => {
                    error!("Failed to reconcile vault {}: {}", vault_id, e);
                    total_inconsistent += 1;
                    total_discrepancies += 1;
                }
            }
        }
        
        Ok(BulkReconciliationResult {
            total_vaults: results.len(),
            consistent_vaults: results.len() - total_inconsistent,
            inconsistent_vaults: total_inconsistent,
            total_discrepancies,
            results,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReconciliationResult {
    pub vault_id: Uuid,
    pub is_consistent: bool,
    pub discrepancies: Vec<Discrepancy>,
    pub database_total: i64,
    pub cached_total: i64,
    pub last_reconciliation: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Discrepancy {
    pub field: String,
    pub database_value: i64,
    pub cached_value: i64,
    pub severity: DiscrepancySeverity,
    pub issue: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiscrepancySeverity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone)]
pub struct BulkReconciliationResult {
    pub total_vaults: usize,
    pub consistent_vaults: usize,
    pub inconsistent_vaults: usize,
    pub total_discrepancies: usize,
    pub results: Vec<ReconciliationResult>,
}