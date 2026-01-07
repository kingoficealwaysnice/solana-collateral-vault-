-- Collateral Vault Management System Database Schema
-- PostgreSQL database for off-chain state management

-- Enable UUID extension
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Enable PostGIS for geographic data (if needed)
-- CREATE EXTENSION IF NOT EXISTS postgis;

-- Vaults table: stores user vault metadata
CREATE TABLE vaults (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_pubkey VARCHAR(44) NOT NULL UNIQUE, -- Solana public key (base58)
    vault_pubkey VARCHAR(44) NOT NULL UNIQUE, -- PDA vault account
    token_account_pubkey VARCHAR(44) NOT NULL UNIQUE, -- PDA token account
    bump SMALLINT NOT NULL CHECK (bump >= 0 AND bump <= 255),
    
    -- Balance tracking (in USDT lamports)
    total_balance BIGINT NOT NULL DEFAULT 0 CHECK (total_balance >= 0),
    locked_balance BIGINT NOT NULL DEFAULT 0 CHECK (locked_balance >= 0),
    available_balance BIGINT NOT NULL DEFAULT 0 CHECK (available_balance >= 0),
    
    -- Status and metadata
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Security and audit fields
    created_by VARCHAR(44), -- Authority that created the vault
    version INTEGER NOT NULL DEFAULT 1,
    
    -- Ensure balance invariant: total = locked + available
    CONSTRAINT balance_invariant CHECK (total_balance = locked_balance + available_balance)
);

-- Index for efficient user lookups
CREATE INDEX idx_vaults_user_pubkey ON vaults(user_pubkey);
CREATE INDEX idx_vaults_vault_pubkey ON vaults(vault_pubkey);
CREATE INDEX idx_vaults_token_account_pubkey ON vaults(token_account_pubkey);
CREATE INDEX idx_vaults_active ON vaults(is_active) WHERE is_active = true;
CREATE INDEX idx_vaults_last_activity ON vaults(last_activity_at);

-- Transaction records: stores all vault operations
CREATE TABLE transaction_records (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    vault_id UUID NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    
    -- Transaction details
    transaction_type VARCHAR(20) NOT NULL CHECK (transaction_type IN ('initialize', 'deposit', 'withdraw', 'lock', 'unlock', 'transfer')),
    amount BIGINT NOT NULL, -- Can be negative for withdrawals/unlocks
    
    -- Solana transaction details
    solana_signature VARCHAR(88) UNIQUE, -- Transaction signature (base58)
    solana_slot BIGINT, -- Block slot
    solana_timestamp TIMESTAMPTZ, -- Block timestamp
    
    -- Status tracking
    status VARCHAR(20) NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'failed', 'expired')),
    error_message TEXT,
    
    -- Metadata
    operation_id UUID, -- For idempotency
    source_vault_id UUID REFERENCES vaults(id), -- For transfers
    destination_vault_id UUID REFERENCES vaults(id), -- For transfers
    
    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    confirmed_at TIMESTAMPTZ,
    
    -- Versioning
    version INTEGER NOT NULL DEFAULT 1
);

-- Indexes for transaction queries
CREATE INDEX idx_transactions_vault_id ON transaction_records(vault_id);
CREATE INDEX idx_transactions_type ON transaction_records(transaction_type);
CREATE INDEX idx_transactions_status ON transaction_records(status);
CREATE INDEX idx_transactions_solana_signature ON transaction_records(solana_signature);
CREATE INDEX idx_transactions_created_at ON transaction_records(created_at);
CREATE INDEX idx_transactions_operation_id ON transaction_records(operation_id);
CREATE INDEX idx_transactions_source_dest ON transaction_records(source_vault_id, destination_vault_id);

-- Balance snapshots: periodic balance state for reconciliation
CREATE TABLE balance_snapshots (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    vault_id UUID NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    
    -- Balance state
    total_balance BIGINT NOT NULL,
    locked_balance BIGINT NOT NULL,
    available_balance BIGINT NOT NULL,
    
    -- Reconciliation details
    block_height BIGINT, -- Solana block height
    block_timestamp TIMESTAMPTZ,
    
    -- Consistency checks
    is_consistent BOOLEAN NOT NULL DEFAULT true,
    discrepancies JSONB, -- Array of discrepancy details
    
    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by VARCHAR(50) DEFAULT 'system', -- 'system', 'manual', 'reconciliation'
    
    -- Ensure balance invariant
    CONSTRAINT snapshot_balance_invariant CHECK (total_balance = locked_balance + available_balance)
);

-- Indexes for snapshot queries
CREATE INDEX idx_snapshots_vault_id ON balance_snapshots(vault_id);
CREATE INDEX idx_snapshots_created_at ON balance_snapshots(created_at);
CREATE INDEX idx_snapshots_block_height ON balance_snapshots(block_height);
CREATE INDEX idx_snapshots_consistent ON balance_snapshots(is_consistent) WHERE is_consistent = false;

-- Pending operations: track operations in progress to prevent race conditions
CREATE TABLE pending_operations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    operation_id UUID NOT NULL UNIQUE,
    
    -- Operation details
    operation_type VARCHAR(20) NOT NULL CHECK (operation_type IN ('lock', 'unlock', 'transfer', 'withdraw')),
    vault_id UUID NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    amount BIGINT NOT NULL CHECK (amount > 0),
    
    -- Status tracking
    status VARCHAR(20) NOT NULL DEFAULT 'in_progress' CHECK (status IN ('in_progress', 'completed', 'failed')),
    
    -- Timing
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '5 minutes'),
    completed_at TIMESTAMPTZ,
    
    -- Metadata
    created_by VARCHAR(50) NOT NULL, -- 'cp_manager', 'api', etc.
    error_message TEXT
);

-- Indexes for pending operations
CREATE INDEX idx_pending_operations_operation_id ON pending_operations(operation_id);
CREATE INDEX idx_pending_operations_vault_id ON pending_operations(vault_id);
CREATE INDEX idx_pending_operations_status ON pending_operations(status);
CREATE INDEX idx_pending_operations_expires_at ON pending_operations(expires_at);

-- System configuration: runtime configuration parameters
CREATE TABLE system_config (
    key VARCHAR(100) PRIMARY KEY,
    value TEXT NOT NULL,
    value_type VARCHAR(20) NOT NULL CHECK (value_type IN ('string', 'integer', 'boolean', 'json')),
    description TEXT,
    is_secret BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Insert default configuration
INSERT INTO system_config (key, value, value_type, description) VALUES
('max_concurrent_transactions', '5', 'integer', 'Maximum concurrent Solana transactions'),
('transaction_retry_limit', '3', 'integer', 'Maximum retry attempts per transaction'),
('reconciliation_interval_seconds', '300', 'integer', 'Interval between balance reconciliations'),
('health_check_interval_seconds', '30', 'integer', 'Interval between health checks'),
('stale_transaction_threshold_seconds', '3600', 'integer', 'Time before pending transactions are considered stale'),
('max_pending_transactions', '100', 'integer', 'Maximum number of pending transactions before alerts'),
('rate_limit_per_second', '10', 'integer', 'Maximum API requests per second'),
('enable_balance_reconciliation', 'true', 'boolean', 'Enable automatic balance reconciliation'),
('enable_health_monitoring', 'true', 'boolean', 'Enable system health monitoring');

-- Audit log: comprehensive audit trail
CREATE TABLE audit_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Event details
    event_type VARCHAR(50) NOT NULL,
    event_category VARCHAR(50) NOT NULL CHECK (event_category IN ('vault', 'transaction', 'balance', 'system', 'security')),
    
    -- Entity references
    vault_id UUID REFERENCES vaults(id),
    transaction_id UUID REFERENCES transaction_records(id),
    user_pubkey VARCHAR(44),
    
    -- Event data
    event_data JSONB NOT NULL,
    
    -- Actor information
    actor_type VARCHAR(20) NOT NULL CHECK (actor_type IN ('user', 'system', 'admin', 'external')),
    actor_id VARCHAR(100), -- Public key or system identifier
    actor_ip INET, -- IP address for API calls
    
    -- Timing
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Integrity
    hash VARCHAR(64) -- SHA-256 hash of event data for tamper detection
);

-- Indexes for audit queries
CREATE INDEX idx_audit_vault_id ON audit_log(vault_id);
CREATE INDEX idx_audit_transaction_id ON audit_log(transaction_id);
CREATE INDEX idx_audit_event_type ON audit_log(event_type);
CREATE INDEX idx_audit_event_category ON audit_log(event_category);
CREATE INDEX idx_audit_created_at ON audit_log(created_at);
CREATE INDEX idx_audit_actor ON audit_log(actor_type, actor_id);

-- System health metrics: performance and health monitoring
CREATE TABLE system_metrics (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Metric details
    metric_name VARCHAR(100) NOT NULL,
    metric_value NUMERIC NOT NULL,
    metric_unit VARCHAR(20),
    
    -- Context
    metric_category VARCHAR(50) NOT NULL CHECK (metric_category IN ('performance', 'health', 'usage', 'error')),
    labels JSONB, -- Additional labels for the metric
    
    -- Timing
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Source
    source VARCHAR(50) NOT NULL DEFAULT 'backend' -- 'backend', 'monitor', 'api', etc.
);

-- Indexes for metrics queries
CREATE INDEX idx_metrics_name ON system_metrics(metric_name);
CREATE INDEX idx_metrics_category ON system_metrics(metric_category);
CREATE INDEX idx_metrics_recorded_at ON system_metrics(recorded_at);

-- Alert rules: configurable alerting thresholds
CREATE TABLE alert_rules (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Rule details
    rule_name VARCHAR(100) NOT NULL UNIQUE,
    rule_description TEXT,
    
    -- Conditions
    metric_name VARCHAR(100) NOT NULL,
    condition_operator VARCHAR(10) NOT NULL CHECK (condition_operator IN ('>', '<', '>=', '<=', '==', '!=')),
    threshold_value NUMERIC NOT NULL,
    
    -- Alerting
    alert_severity VARCHAR(20) NOT NULL CHECK (alert_severity IN ('info', 'warning', 'critical')),
    notification_channels JSONB, -- Array of notification channels
    
    -- Status
    is_enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Insert default alert rules
INSERT INTO alert_rules (rule_name, rule_description, metric_name, condition_operator, threshold_value, alert_severity) VALUES
('high_failed_transactions', 'High rate of failed transactions', 'failed_transactions_rate', '>', 0.1, 'warning'),
('critical_balance_invariant', 'Balance invariant violations detected', 'balance_invariant_violations', '>', 0, 'critical'),
('high_pending_transactions', 'Too many pending transactions', 'pending_transactions_count', '>', 50, 'warning'),
('system_unhealthy', 'System health check failed', 'system_health_score', '<', 0.8, 'critical'),
('database_connection_failed', 'Database connection issues', 'database_connection_status', '==', 0, 'critical');

-- Vault access control: fine-grained permissions (for future multi-tenant support)
CREATE TABLE vault_permissions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    vault_id UUID NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    
    -- Permission details
    permission_type VARCHAR(50) NOT NULL CHECK (permission_type IN ('view', 'deposit', 'withdraw', 'lock', 'unlock', 'admin')),
    
    -- Actor
    actor_pubkey VARCHAR(44) NOT NULL, -- Solana public key
    actor_type VARCHAR(20) NOT NULL CHECK (actor_type IN ('user', 'program', 'admin')),
    
    -- Status
    is_active BOOLEAN NOT NULL DEFAULT true,
    expires_at TIMESTAMPTZ,
    
    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by VARCHAR(44), -- Who granted the permission
    
    -- Ensure unique active permissions per vault/actor/permission type
    CONSTRAINT unique_active_permission UNIQUE (vault_id, actor_pubkey, permission_type, is_active) WHERE is_active = true
);

-- Indexes for permission queries
CREATE INDEX idx_permissions_vault_id ON vault_permissions(vault_id);
CREATE INDEX idx_permissions_actor ON vault_permissions(actor_pubkey);
CREATE INDEX idx_permissions_type ON vault_permissions(permission_type);
CREATE INDEX idx_permissions_active ON vault_permissions(is_active) WHERE is_active = true;

-- Create a function to update the updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Create triggers for updated_at columns
CREATE TRIGGER update_vaults_updated_at BEFORE UPDATE ON vaults
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_transaction_records_updated_at BEFORE UPDATE ON transaction_records
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_system_config_updated_at BEFORE UPDATE ON system_config
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_alert_rules_updated_at BEFORE UPDATE ON alert_rules
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_vault_permissions_updated_at BEFORE UPDATE ON vault_permissions
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- Create a function to audit vault changes
CREATE OR REPLACE FUNCTION audit_vault_change()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO audit_log (event_type, event_category, vault_id, user_pubkey, event_data, actor_type, actor_id)
    VALUES (
        CASE 
            WHEN TG_OP = 'INSERT' THEN 'vault_created'
            WHEN TG_OP = 'UPDATE' THEN 'vault_updated'
            WHEN TG_OP = 'DELETE' THEN 'vault_deleted'
        END,
        'vault',
        NEW.id,
        NEW.user_pubkey,
        jsonb_build_object(
            'old_total_balance', COALESCE(OLD.total_balance, 0),
            'new_total_balance', NEW.total_balance,
            'old_locked_balance', COALESCE(OLD.locked_balance, 0),
            'new_locked_balance', NEW.locked_balance,
            'old_available_balance', COALESCE(OLD.available_balance, 0),
            'new_available_balance', NEW.available_balance,
            'old_is_active', COALESCE(OLD.is_active, true),
            'new_is_active', NEW.is_active
        ),
        'system',
        'system'
    );
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Create trigger for vault auditing
CREATE TRIGGER audit_vault_changes AFTER INSERT OR UPDATE OR DELETE ON vaults
    FOR EACH ROW EXECUTE FUNCTION audit_vault_change();

-- Create a function to audit transaction changes
CREATE OR REPLACE FUNCTION audit_transaction_change()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO audit_log (event_type, event_category, vault_id, transaction_id, user_pubkey, event_data, actor_type, actor_id)
    VALUES (
        CASE 
            WHEN TG_OP = 'INSERT' THEN 'transaction_created'
            WHEN TG_OP = 'UPDATE' THEN 'transaction_updated'
            WHEN TG_OP = 'DELETE' THEN 'transaction_deleted'
        END,
        'transaction',
        NEW.vault_id,
        NEW.id,
        (SELECT user_pubkey FROM vaults WHERE id = NEW.vault_id),
        jsonb_build_object(
            'transaction_type', NEW.transaction_type,
            'amount', NEW.amount,
            'old_status', COALESCE(OLD.status, ''),
            'new_status', NEW.status,
            'solana_signature', COALESCE(NEW.solana_signature, '')
        ),
        'system',
        'system'
    );
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Create trigger for transaction auditing
CREATE TRIGGER audit_transaction_changes AFTER INSERT OR UPDATE OR DELETE ON transaction_records
    FOR EACH ROW EXECUTE FUNCTION audit_transaction_change();

-- Create views for common queries

-- Vault summary view
CREATE VIEW vault_summary AS
SELECT 
    v.id,
    v.user_pubkey,
    v.vault_pubkey,
    v.token_account_pubkey,
    v.total_balance,
    v.locked_balance,
    v.available_balance,
    v.is_active,
    v.created_at,
    v.last_activity_at,
    COUNT(t.id) as transaction_count,
    MAX(t.created_at) as last_transaction_at
FROM vaults v
LEFT JOIN transaction_records t ON v.id = t.vault_id
GROUP BY v.id;

-- Transaction summary view
CREATE VIEW transaction_summary AS
SELECT 
    t.id,
    t.vault_id,
    v.user_pubkey,
    t.transaction_type,
    t.amount,
    t.status,
    t.solana_signature,
    t.created_at,
    t.confirmed_at,
    CASE 
        WHEN t.confirmed_at IS NOT NULL THEN EXTRACT(EPOCH FROM (t.confirmed_at - t.created_at))
        ELSE NULL
    END as confirmation_time_seconds
FROM transaction_records t
JOIN vaults v ON t.vault_id = v.id;

-- System health view
CREATE VIEW system_health AS
SELECT 
    (SELECT COUNT(*) FROM vaults WHERE is_active = true) as active_vaults,
    (SELECT COUNT(*) FROM transaction_records WHERE status = 'pending') as pending_transactions,
    (SELECT COUNT(*) FROM transaction_records WHERE status = 'failed' AND created_at > NOW() - INTERVAL '1 hour') as failed_transactions_1h,
    (SELECT COUNT(*) FROM pending_operations WHERE status = 'in_progress') as pending_operations,
    (SELECT COUNT(*) FROM balance_snapshots WHERE is_consistent = false) as inconsistent_snapshots,
    (SELECT SUM(total_balance) FROM vaults WHERE is_active = true) as total_value_locked,
    NOW() as checked_at;

-- Create a function to clean up old data
CREATE OR REPLACE FUNCTION cleanup_old_data()
RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER := 0;
BEGIN
    -- Delete old audit logs (keep 90 days)
    DELETE FROM audit_log WHERE created_at < NOW() - INTERVAL '90 days';
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    
    -- Delete old system metrics (keep 30 days)
    DELETE FROM system_metrics WHERE recorded_at < NOW() - INTERVAL '30 days';
    
    -- Delete old balance snapshots (keep 7 days)
    DELETE FROM balance_snapshots WHERE created_at < NOW() - INTERVAL '7 days';
    
    -- Clean up expired pending operations
    DELETE FROM pending_operations WHERE expires_at < NOW();
    
    RETURN deleted_count;
END;
$$ language 'plpgsql';