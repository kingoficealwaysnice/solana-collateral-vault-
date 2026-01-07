#!/bin/bash

# Collateral Vault System Test Runner
# This script runs all tests for the collateral vault management system

set -e

echo "🧪 Starting Collateral Vault System Test Suite"
echo "=============================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to run tests and report results
run_test_suite() {
    local test_name=$1
    local test_command=$2
    
    echo -e "${YELLOW}Running $test_name...${NC}"
    
    if $test_command; then
        echo -e "${GREEN}✅ $test_name passed${NC}"
        return 0
    else
        echo -e "${RED}❌ $test_name failed${NC}"
        return 1
    fi
}

# Check if PostgreSQL is running
check_postgres() {
    echo "🔍 Checking PostgreSQL connection..."
    
    if pg_isready -h localhost -p 5432 >/dev/null 2>&1; then
        echo -e "${GREEN}✅ PostgreSQL is running${NC}"
        return 0
    else
        echo -e "${RED}❌ PostgreSQL is not running${NC}"
        echo "Please start PostgreSQL and ensure it's accessible at localhost:5432"
        return 1
    fi
}

# Set up test database
setup_test_db() {
    echo "🗄️ Setting up test database..."
    
    # Create test database if it doesn't exist
    createdb vault_test_db 2>/dev/null || echo "Test database already exists"
    
    # Run migrations on test database
    cd src
    DATABASE_URL="postgres://user:password@localhost/vault_test_db" sqlx migrate run
    cd ..
    
    echo -e "${GREEN}✅ Test database setup complete${NC}"
}

# Run Anchor contract tests
test_anchor_contract() {
    echo "🔗 Running Anchor contract tests..."
    cd programs/collateral-vault
    
    if anchor test; then
        echo -e "${GREEN}✅ Anchor contract tests passed${NC}"
        cd ../..
        return 0
    else
        echo -e "${RED}❌ Anchor contract tests failed${NC}"
        cd ../..
        return 1
    fi
}

# Run Rust backend unit tests
test_backend_unit() {
    echo "🔧 Running Rust backend unit tests..."
    cd src
    
    if cargo test --test unit_tests; then
        echo -e "${GREEN}✅ Backend unit tests passed${NC}"
        cd ..
        return 0
    else
        echo -e "${RED}❌ Backend unit tests failed${NC}"
        cd ..
        return 1
    fi
}

# Run Rust backend integration tests
test_backend_integration() {
    echo "🔗 Running Rust backend integration tests..."
    cd src
    
    if cargo test --test integration_tests; then
        echo -e "${GREEN}✅ Backend integration tests passed${NC}"
        cd ..
        return 0
    else
        echo -e "${RED}❌ Backend integration tests failed${NC}"
        cd ..
        return 1
    fi
}

# Run Rust backend security tests
test_backend_security() {
    echo "🔒 Running Rust backend security tests..."
    cd src
    
    if cargo test --test security_tests; then
        echo -e "${GREEN}✅ Backend security tests passed${NC}"
        cd ..
        return 0
    else
        echo -e "${RED}❌ Backend security tests failed${NC}"
        cd ..
        return 1
    fi
}

# Run all tests
run_all_tests() {
    local failed_tests=0
    
    echo "🚀 Starting comprehensive test suite"
    echo "======================================"
    
    # Check prerequisites
    if ! check_postgres; then
        return 1
    fi
    
    setup_test_db
    
    # Run test suites
    if ! run_test_suite "Anchor Contract Tests" test_anchor_contract; then
        ((failed_tests++))
    fi
    
    if ! run_test_suite "Backend Unit Tests" test_backend_unit; then
        ((failed_tests++))
    fi
    
    if ! run_test_suite "Backend Integration Tests" test_backend_integration; then
        ((failed_tests++))
    fi
    
    if ! run_test_suite "Backend Security Tests" test_backend_security; then
        ((failed_tests++))
    fi
    
    echo "======================================"
    if [ $failed_tests -eq 0 ]; then
        echo -e "${GREEN}🎉 All tests passed!${NC}"
        return 0
    else
        echo -e "${RED}💥 $failed_tests test suite(s) failed${NC}"
        return 1
    fi
}

# Run specific test category
run_specific_tests() {
    local test_category=$1
    
    case $test_category in
        "contract")
            check_postgres && setup_test_db && test_anchor_contract
            ;;
        "unit")
            check_postgres && setup_test_db && test_backend_unit
            ;;
        "integration")
            check_postgres && setup_test_db && test_backend_integration
            ;;
        "security")
            check_postgres && setup_test_db && test_backend_security
            ;;
        *)
            echo "Unknown test category: $test_category"
            echo "Available categories: contract, unit, integration, security"
            return 1
            ;;
    esac
}

# Main execution
main() {
    local test_category=${1:-all}
    
    case $test_category in
        "all")
            run_all_tests
            ;;
        "contract"|"unit"|"integration"|"security")
            run_specific_tests $test_category
            ;;
        *)
            echo "Usage: $0 [all|contract|unit|integration|security]"
            echo ""
            echo "Test categories:"
            echo "  all          - Run all test suites (default)"
            echo "  contract     - Run Anchor contract tests only"
            echo "  unit         - Run Rust backend unit tests only"
            echo "  integration  - Run Rust backend integration tests only"
            echo "  security     - Run Rust backend security tests only"
            return 1
            ;;
    esac
}

# Run main function with all arguments
main "$@"