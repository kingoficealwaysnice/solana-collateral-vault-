-- Add idempotency support for API requests
ALTER TABLE transaction_records ADD COLUMN idempotency_key VARCHAR(100);
CREATE INDEX idx_transactions_idempotency_key ON transaction_records(idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Add transaction priority support
ALTER TABLE transaction_records ADD COLUMN priority INTEGER NOT NULL DEFAULT 1 CHECK (priority >= 1 AND priority <= 10);
CREATE INDEX idx_transactions_priority ON transaction_records(priority) WHERE status = 'pending';

-- Add transaction metadata for complex operations
ALTER TABLE transaction_records ADD COLUMN metadata JSONB;

-- Add vault tags for categorization
ALTER TABLE vaults ADD COLUMN tags TEXT[] DEFAULT '{}';
CREATE INDEX idx_vaults_tags ON vaults USING GIN (tags);

-- Add vault risk assessment
ALTER TABLE vaults ADD COLUMN risk_score INTEGER DEFAULT 0 CHECK (risk_score >= 0 AND risk_score <= 100);
ALTER TABLE vaults ADD COLUMN risk_factors JSONB DEFAULT '{}';

-- Create a table for vault state transitions
CREATE TABLE vault_state_transitions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    vault_id UUID NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    
    -- State transition details
    from_state VARCHAR(50) NOT NULL,
    to_state VARCHAR(50) NOT NULL,
    transition_reason VARCHAR(200),
    
    -- Timing
    transitioned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Actor
    transitioned_by VARCHAR(44) NOT NULL,
    transitioned_by_type VARCHAR(20) NOT NULL CHECK (transitioned_by_type IN ('user', 'system', 'admin', 'external'))
);

CREATE INDEX idx_state_transitions_vault_id ON vault_state_transitions(vault_id);
CREATE INDEX idx_state_transitions_transitioned_at ON vault_state_transitions(transitioned_at);

-- Create a function to track vault state transitions
CREATE OR REPLACE FUNCTION track_vault_state_transition()
RETURNS TRIGGER AS $$
BEGIN
    IF OLD.is_active != NEW.is_active THEN
        INSERT INTO vault_state_transitions (
            vault_id, from_state, to_state, transition_reason, transitioned_by, transitioned_by_type
        ) VALUES (
            NEW.id,
            CASE WHEN OLD.is_active THEN 'active' ELSE 'inactive' END,
            CASE WHEN NEW.is_active THEN 'active' ELSE 'inactive' END,
            'is_active changed',
            'system',
            'system'
        );
    END IF;
    
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Create trigger for vault state transitions
CREATE TRIGGER track_vault_state_changes AFTER UPDATE ON vaults
    FOR EACH ROW EXECUTE FUNCTION track_vault_state_transition();

-- Create a table for transaction dependencies
CREATE TABLE transaction_dependencies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Transaction relationship
    dependent_transaction_id UUID NOT NULL REFERENCES transaction_records(id) ON DELETE CASCADE,
    prerequisite_transaction_id UUID NOT NULL REFERENCES transaction_records(id) ON DELETE CASCADE,
    
    -- Dependency type
    dependency_type VARCHAR(50) NOT NULL CHECK (dependency_type IN ('sequential', 'concurrent', 'exclusive')),
    
    -- Status
    is_resolved BOOLEAN NOT NULL DEFAULT false,
    resolved_at TIMESTAMPTZ,
    
    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Ensure no circular dependencies
    CONSTRAINT unique_dependency UNIQUE (dependent_transaction_id, prerequisite_transaction_id)
);

CREATE INDEX idx_dependencies_dependent ON transaction_dependencies(dependent_transaction_id);
CREATE INDEX idx_dependencies_prerequisite ON transaction_dependencies(prerequisite_transaction_id);
CREATE INDEX idx_dependencies_resolved ON transaction_dependencies(is_resolved) WHERE is_resolved = false;

-- Create a function to check transaction dependencies
CREATE OR REPLACE FUNCTION check_transaction_dependencies(p_transaction_id UUID)
RETURNS TABLE (
    can_proceed BOOLEAN,
    blocking_transaction_id UUID,
    blocking_reason TEXT
) AS $$
BEGIN
    RETURN QUERY
    SELECT 
        CASE 
            WHEN COUNT(*) = 0 THEN true
            WHEN bool_and(td.is_resolved) THEN true
            ELSE false
        END as can_proceed,
        CASE 
            WHEN bool_and(td.is_resolved) THEN NULL
            ELSE (SELECT prerequisite_transaction_id FROM transaction_dependencies WHERE dependent_transaction_id = p_transaction_id AND is_resolved = false LIMIT 1)
        END as blocking_transaction_id,
        CASE 
            WHEN bool_and(td.is_resolved) THEN NULL
            ELSE 'Waiting for prerequisite transaction to complete'
        END as blocking_reason
    FROM transaction_dependencies td
    WHERE td.dependent_transaction_id = p_transaction_id;
END;
$$ language 'plpgsql';

-- Create a table for rate limiting
CREATE TABLE rate_limit_buckets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Rate limit identifier
    bucket_key VARCHAR(200) NOT NULL UNIQUE, -- e.g., "user:pubkey:operation_type"
    
    -- Current state
    current_tokens INTEGER NOT NULL DEFAULT 0,
    max_tokens INTEGER NOT NULL CHECK (max_tokens > 0),
    refill_rate INTEGER NOT NULL CHECK (refill_rate > 0), -- tokens per second
    
    -- Timing
    last_refill_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    
    -- Metadata
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_rate_limit_buckets_key ON rate_limit_buckets(bucket_key);
CREATE INDEX idx_rate_limit_buckets_expires ON rate_limit_buckets(expires_at) WHERE expires_at IS NOT NULL;

-- Create a function to check and consume rate limit tokens
CREATE OR REPLACE FUNCTION consume_rate_limit_token(
    p_bucket_key VARCHAR(200),
    p_tokens_to_consume INTEGER DEFAULT 1,
    p_max_tokens INTEGER DEFAULT 10,
    p_refill_rate INTEGER DEFAULT 1
)
RETURNS TABLE (
    allowed BOOLEAN,
    remaining_tokens INTEGER,
    reset_at TIMESTAMPTZ
) AS $$
DECLARE
    v_current_tokens INTEGER;
    v_last_refill TIMESTAMPTZ;
    v_seconds_since_refill INTEGER;
    v_tokens_to_add INTEGER;
    v_new_tokens INTEGER;
BEGIN
    -- Get or create rate limit bucket
    INSERT INTO rate_limit_buckets (bucket_key, max_tokens, refill_rate)
    VALUES (p_bucket_key, p_max_tokens, p_refill_rate)
    ON CONFLICT (bucket_key) DO NOTHING;
    
    -- Calculate current tokens after refill
    SELECT current_tokens, last_refill_at INTO v_current_tokens, v_last_refill
    FROM rate_limit_buckets WHERE bucket_key = p_bucket_key;
    
    v_seconds_since_refill := EXTRACT(EPOCH FROM (NOW() - v_last_refill));
    v_tokens_to_add := v_seconds_since_refill * p_refill_rate;
    v_new_tokens := LEAST(v_current_tokens + v_tokens_to_add, p_max_tokens);
    
    -- Update bucket state
    UPDATE rate_limit_buckets 
    SET current_tokens = v_new_tokens - CASE WHEN v_new_tokens >= p_tokens_to_consume THEN p_tokens_to_consume ELSE 0 END,
        last_refill_at = NOW(),
        updated_at = NOW()
    WHERE bucket_key = p_bucket_key;
    
    -- Return result
    RETURN QUERY
    SELECT 
        v_new_tokens >= p_tokens_to_consume,
        v_new_tokens - CASE WHEN v_new_tokens >= p_tokens_to_consume THEN p_tokens_to_consume ELSE 0 END,
        NOW() + INTERVAL '1 second' * ((p_max_tokens - v_new_tokens + p_tokens_to_consume) / p_refill_rate);
END;
$$ language 'plpgsql';

-- Create a table for API keys (for external integrations)
CREATE TABLE api_keys (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Key details
    key_hash VARCHAR(128) NOT NULL UNIQUE, -- SHA-256 hash of the API key
    key_prefix VARCHAR(20) NOT NULL, -- First 8 characters for identification
    
    -- Permissions
    permissions TEXT[] NOT NULL DEFAULT '{}',
    rate_limit_per_second INTEGER NOT NULL DEFAULT 10,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 100,
    rate_limit_per_hour INTEGER NOT NULL DEFAULT 1000,
    
    -- Status
    is_active BOOLEAN NOT NULL DEFAULT true,
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    
    -- Metadata
    name VARCHAR(100) NOT NULL,
    description TEXT,
    created_by VARCHAR(100) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_api_keys_key_prefix ON api_keys(key_prefix);
CREATE INDEX idx_api_keys_active ON api_keys(is_active) WHERE is_active = true;
CREATE INDEX idx_api_keys_expires ON api_keys(expires_at) WHERE expires_at IS NOT NULL;

-- Create a function to generate API key hash
CREATE OR REPLACE FUNCTION generate_api_key_hash(p_api_key VARCHAR(100))
RETURNS TABLE (
    key_hash VARCHAR(128),
    key_prefix VARCHAR(20)
) AS $$
BEGIN
    RETURN QUERY
    SELECT 
        encode(sha256(p_api_key::bytea), 'hex'),
        SUBSTRING(p_api_key FROM 1 FOR 8);
END;
$$ language 'plpgsql';

-- Create a table for webhook subscriptions
CREATE TABLE webhook_subscriptions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Subscription details
    url VARCHAR(500) NOT NULL,
    secret VARCHAR(100), -- For HMAC signature verification
    
    -- Events to subscribe to
    events TEXT[] NOT NULL DEFAULT '{}', -- e.g., ['vault.created', 'transaction.confirmed']
    
    -- Status
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_delivery_at TIMESTAMPTZ,
    last_delivery_status VARCHAR(50),
    failure_count INTEGER NOT NULL DEFAULT 0,
    
    -- Retry configuration
    max_retries INTEGER NOT NULL DEFAULT 3,
    retry_delay_seconds INTEGER NOT NULL DEFAULT 60,
    
    -- Metadata
    name VARCHAR(100) NOT NULL,
    description TEXT,
    created_by VARCHAR(100) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_webhook_subscriptions_active ON webhook_subscriptions(is_active) WHERE is_active = true;
CREATE INDEX idx_webhook_subscriptions_events ON webhook_subscriptions USING GIN (events);

-- Create a table for webhook deliveries
CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    
    -- Delivery details
    subscription_id UUID NOT NULL REFERENCES webhook_subscriptions(id) ON DELETE CASCADE,
    event_type VARCHAR(100) NOT NULL,
    event_data JSONB NOT NULL,
    
    -- Delivery status
    status VARCHAR(50) NOT NULL CHECK (status IN ('pending', 'delivered', 'failed', 'expired')),
    response_code INTEGER,
    response_body TEXT,
    error_message TEXT,
    
    -- Retry tracking
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    
    -- Timing
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at TIMESTAMPTZ,
    
    -- Request/response tracking
    request_headers JSONB,
    response_headers JSONB
);

CREATE INDEX idx_webhook_deliveries_subscription ON webhook_deliveries(subscription_id);
CREATE INDEX idx_webhook_deliveries_status ON webhook_deliveries(status);
CREATE INDEX idx_webhook_deliveries_next_retry ON webhook_deliveries(next_retry_at) WHERE status = 'failed' AND next_retry_at IS NOT NULL;

-- Create a function to create webhook delivery
CREATE OR REPLACE FUNCTION create_webhook_delivery(
    p_subscription_id UUID,
    p_event_type VARCHAR(100),
    p_event_data JSONB
)
RETURNS UUID AS $$
DECLARE
    v_delivery_id UUID;
BEGIN
    INSERT INTO webhook_deliveries (subscription_id, event_type, event_data)
    VALUES (p_subscription_id, p_event_type, p_event_data)
    RETURNING id INTO v_delivery_id;
    
    RETURN v_delivery_id;
END;
$$ language 'plpgsql';