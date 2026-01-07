# Collateral Vault System Test Documentation

## Overview

The Collateral Vault Management System includes a comprehensive test suite covering unit tests, integration tests, and security tests. This documentation provides an overview of the testing strategy, test categories, and how to run the tests.

## Test Architecture

### Test Categories

1. **Unit Tests** (`tests/unit_tests.rs`)
   - Test individual components in isolation
   - Mock external dependencies (Solana RPC, database)
   - Focus on business logic correctness

2. **Integration Tests** (`tests/integration_tests.rs`)
   - Test component interactions
   - Use real database connections
   - Test API endpoints and WebSocket functionality

3. **Security Tests** (`tests/security_tests.rs`)
   - Test security vulnerabilities
   - Adversarial input testing
   - Race condition testing
   - Injection attack prevention

### Test Structure

```
tests/
├── unit_tests.rs          # Unit tests for core modules
├── integration_tests.rs   # API and integration tests
└── security_tests.rs    # Security and adversarial tests
```

## Unit Tests

### Test Coverage

#### VaultManager Tests
- `test_vault_creation`: Tests vault creation with valid parameters
- `test_duplicate_vault_creation`: Tests handling of duplicate vaults
- `test_vault_balance_updates`: Tests balance update functionality

#### BalanceTracker Tests
- `test_balance_snapshot_recording`: Tests balance snapshot creation
- `test_balance_snapshot_retrieval`: Tests historical balance queries

#### TransactionManager Tests
- `test_transaction_recording`: Tests transaction recording
- `test_transaction_status_updates`: Tests transaction status changes
- `test_idempotency_key_handling`: Tests idempotent operations

#### RateLimitRepository Tests
- `test_rate_limit_repository`: Tests token bucket algorithm
- `test_rate_limit_reset`: Tests token refill mechanism

## Integration Tests

### API Endpoint Tests

#### Health Check Endpoint
- `test_health_check`: Tests basic API availability
- `test_rate_limiting`: Tests rate limiting functionality

#### Vault Management Endpoints
- `test_create_vault`: Tests vault creation via API
- `test_get_vault`: Tests vault retrieval
- `test_vault_not_found`: Tests error handling for non-existent vaults

#### Transaction Endpoints
- `test_deposit_transaction`: Tests deposit functionality
- `test_withdraw_transaction`: Tests withdraw functionality
- `test_idempotency_key_usage`: Tests idempotent operations

#### WebSocket Tests
- `test_websocket_connection`: Tests WebSocket connectivity
- `test_websocket_message_handling`: Tests message broadcasting

## Security Tests

### Security Vulnerability Tests

#### SQL Injection Prevention
- `test_sql_injection_prevention`: Tests SQL injection resistance
- Tests malicious payloads in user_pubkey parameter
- Verifies proper input sanitization

#### Cross-Site Scripting (XSS) Prevention
- `test_xss_prevention`: Tests XSS payload handling
- Verifies output encoding in API responses
- Tests script injection attempts

#### Rate Limiting Bypass Attempts
- `test_rate_limiting_bypass_attempts`: Tests rate limiting robustness
- Tests different client identifiers
- Verifies per-client rate limiting

#### Authorization Bypass Attempts
- `test_authorization_bypass_attempts`: Tests authentication security
- Tests various authorization header manipulations
- Verifies proper access control

#### Path Traversal Prevention
- `test_path_traversal_attempts`: Tests path traversal attacks
- Tests various encoded path sequences
- Verifies proper URL validation

#### Large Payload Handling
- `test_large_payload_handling`: Tests DoS protection
- Tests extremely large request bodies
- Verifies proper payload size limits

#### Malformed JSON Handling
- `test_malformed_json_handling`: Tests JSON parsing security
- Tests incomplete/malformed JSON payloads
- Verifies proper error handling

### Adversarial Tests

#### Balance Invariant Manipulation
- `test_balance_invariant_manipulation_attempts`: Tests balance integrity
- Tests concurrent balance updates
- Verifies balance invariant enforcement

#### Negative Balance Attempts
- `test_negative_balance_attempts`: Tests negative balance prevention
- Tests negative balance update attempts
- Verifies balance validation

#### Integer Overflow/Underflow
- `test_overflow_underflow_attempts`: Tests arithmetic safety
- Tests extremely large balance values
- Verifies proper overflow handling

#### Race Condition Testing
- `test_race_condition_in_balance_updates`: Tests concurrent access
- Tests simultaneous balance modifications
- Verifies consistency under load

#### Transaction Replay Attacks
- `test_transaction_replay_attacks`: Tests transaction uniqueness
- Tests duplicate transaction signatures
- Verifies replay attack prevention

#### Idempotency Key Manipulation
- `test_idempotency_key_manipulation`: Tests idempotency robustness
- Tests key reuse with different parameters
- Verifies proper idempotency enforcement

## Running Tests

### Prerequisites

1. **PostgreSQL Database**: Ensure PostgreSQL is running locally
2. **Test Database**: Create `vault_test_db` database
3. **Environment Variables**: Set `TEST_DATABASE_URL` if different from default

### Test Execution

#### Run All Tests
```bash
./run_tests.sh
```

#### Run Specific Test Categories
```bash
# Run only contract tests
./run_tests.sh contract

# Run only unit tests
./run_tests.sh unit

# Run only integration tests
./run_tests.sh integration

# Run only security tests
./run_tests.sh security
```

#### Run Tests with Cargo Directly
```bash
# Unit tests
cd src && cargo test --test unit_tests

# Integration tests
cd src && cargo test --test integration_tests

# Security tests
cd src && cargo test --test security_tests
```

### Test Configuration

#### Environment Variables
- `TEST_DATABASE_URL`: PostgreSQL connection string (default: `postgres://user:password@localhost/vault_test_db`)
- `RUST_LOG`: Logging level for tests

#### Test Database Setup
The test runner automatically sets up the test database with required migrations.

## Test Data and Fixtures

### Mock Data
- **User Pubkeys**: Generated using `solana_sdk::signature::Keypair::new()`
- **Vault Pubkeys**: Derived from user pubkeys using PDA derivation
- **Token Accounts**: Mock SPL token accounts
- **Transaction Signatures**: Mock transaction signatures

### Test Scenarios
- **Normal Operations**: Standard deposit/withdraw operations
- **Edge Cases**: Zero amounts, maximum values, empty inputs
- **Error Conditions**: Invalid pubkeys, insufficient balances, network errors
- **Security Scenarios**: Malicious inputs, concurrent access, replay attacks

## Performance Testing

### Load Testing
- **Concurrent Requests**: Tests handle up to 50 concurrent requests
- **Rate Limiting**: Tests handle 100+ requests per second
- **Database Load**: Tests with 10,000+ vault records

### Scalability Testing
- **Vault Creation**: Tests creation of thousands of vaults
- **Transaction Processing**: Tests high-volume transaction processing
- **Balance Tracking**: Tests balance snapshot performance

## Security Testing Methodology

### Attack Surface Analysis
1. **API Endpoints**: All REST endpoints tested for vulnerabilities
2. **WebSocket Connections**: Connection security and message validation
3. **Database Operations**: SQL injection and data integrity tests
4. **Input Validation**: Malformed data and boundary condition testing

### Security Controls Testing
1. **Authentication**: API key and authorization header validation
2. **Authorization**: Access control and privilege escalation prevention
3. **Input Sanitization**: XSS and injection attack prevention
4. **Rate Limiting**: DoS protection and abuse prevention

### Adversarial Testing
1. **Business Logic Attacks**: Tests for logic flaws in balance management
2. **Race Conditions**: Concurrent access and timing attack testing
3. **Replay Attacks**: Transaction uniqueness and replay prevention
4. **State Manipulation**: Tests for unauthorized state changes

## Test Results and Reporting

### Test Output
- **Success/Failure**: Clear pass/fail status for each test
- **Error Messages**: Detailed error information for failed tests
- **Performance Metrics**: Execution time and resource usage
- **Coverage Reports**: Code coverage analysis (when available)

### Continuous Integration
The test suite is designed to run in CI/CD environments with:
- **Docker Support**: Containerized test execution
- **Parallel Execution**: Tests can run in parallel where safe
- **Database Isolation**: Each test uses isolated database state
- **Mock Services**: External services are mocked for reliability

## Best Practices

### Test Writing Guidelines
1. **Isolation**: Tests should be independent and not rely on execution order
2. **Deterministic**: Tests should produce consistent results
3. **Fast**: Unit tests should complete quickly
4. **Comprehensive**: Test both success and failure scenarios
5. **Maintainable**: Tests should be easy to understand and modify

### Security Test Guidelines
1. **Realistic Attacks**: Use realistic attack vectors and payloads
2. **Boundary Testing**: Test edge cases and boundary conditions
3. **Concurrent Testing**: Test race conditions and concurrent access
4. **Error Handling**: Verify proper error handling and logging
5. **Recovery Testing**: Test system recovery from attacks

## Troubleshooting

### Common Issues
1. **Database Connection**: Ensure PostgreSQL is running and accessible
2. **Port Conflicts**: Ensure test ports are available
3. **Solana RPC**: Mock Solana RPC for offline testing
4. **Environment Variables**: Verify required environment variables

### Debug Mode
Enable debug logging:
```bash
RUST_LOG=debug cargo test --test <test_name>
```

### Test Database Issues
Reset test database:
```bash
dropdb vault_test_db && createdb vault_test_db
cd src && DATABASE_URL="postgres://user:password@localhost/vault_test_db" sqlx migrate run
```

## Conclusion

The comprehensive test suite ensures the Collateral Vault Management System is secure, reliable, and production-ready. Regular execution of all test categories is essential for maintaining system integrity and catching regressions early in the development process.