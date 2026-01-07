use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Mint};
use solana_program_test::*;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
    system_instruction,
};
use std::str::FromStr;

use collateral_vault::{
    self,
    accounts::{InitializeVault, Deposit, Withdraw, LockCollateral, UnlockCollateral, TransferCollateral},
    instruction,
    Vault, VaultError,
};

const USDT_MINT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";

#[tokio::test]
async fn test_initialize_vault() {
    let program = ProgramTest::new("collateral_vault", collateral_vault::id(), processor!(collateral_vault::entry));
    let (mut banks_client, payer, recent_blockhash) = program.start().await;
    
    let user = Keypair::new();
    let authority = Keypair::new();
    let usdt_mint = Pubkey::from_str(USDT_MINT).unwrap();
    
    // Create user account
    let create_user_ix = system_instruction::create_account(
        &payer.pubkey(),
        &user.pubkey(),
        1000000000,
        0,
        &system_program::id(),
    );
    
    // Initialize vault
    let (vault_pda, vault_bump) = Pubkey::find_program_address(
        &[b"vault", user.pubkey().as_ref()],
        &collateral_vault::id(),
    );
    
    let (token_pda, _) = Pubkey::find_program_address(
        &[b"token", vault_pda.as_ref()],
        &collateral_vault::id(),
    );
    
    let init_ix = instruction::initialize_vault(
        collateral_vault::id(),
        vault_bump,
        InitializeVault {
            vault: vault_pda,
            vault_token_account: token_pda,
            user: user.pubkey(),
            authority: authority.pubkey(),
            usdt_mint,
            token_program: token::id(),
            system_program: system_program::id(),
            rent: rent::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[create_user_ix, init_ix],
        Some(&payer.pubkey()),
        &[&payer, &user],
        recent_blockhash,
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    
    // Verify vault state
    let vault_account = banks_client.get_account(vault_pda).await.unwrap().unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_ref()).unwrap();
    
    assert_eq!(vault.user, user.pubkey());
    assert_eq!(vault.token_account, token_pda);
    assert_eq!(vault.total_balance, 0);
    assert_eq!(vault.locked_balance, 0);
    assert_eq!(vault.available_balance, 0);
    assert!(vault.is_active);
}

#[tokio::test]
async fn test_deposit_withdraw_flow() {
    let program = ProgramTest::new("collateral_vault", collateral_vault::id(), processor!(collateral_vault::entry));
    let (mut banks_client, payer, recent_blockhash) = program.start().await;
    
    let user = Keypair::new();
    let authority = Keypair::new();
    let usdt_mint = Pubkey::from_str(USDT_MINT).unwrap();
    
    // Setup vault and token accounts
    let (vault_pda, vault_bump) = setup_vault(&mut banks_client, &payer, &user, &authority, usdt_mint).await;
    
    // Create user USDT account
    let user_usdt_account = create_token_account(&mut banks_client, &payer, usdt_mint, user.pubkey()).await;
    
    // Mint USDT to user
    mint_tokens(&mut banks_client, &payer, usdt_mint, user_usdt_account, 1000000000).await;
    
    // Deposit 1000 USDT
    let deposit_amount = 1000000000u64; // 1000 USDT with 6 decimals
    
    let deposit_ix = instruction::deposit(
        collateral_vault::id(),
        deposit_amount,
        Deposit {
            vault: vault_pda,
            vault_token_account: get_vault_token_account(&mut banks_client, vault_pda).await,
            user_token_account: user_usdt_account,
            user: user.pubkey(),
            token_program: token::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&payer.pubkey()),
        &[&payer, &user],
        recent_blockhash,
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    
    // Verify deposit
    let vault_account = banks_client.get_account(vault_pda).await.unwrap().unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_ref()).unwrap();
    
    assert_eq!(vault.total_balance, deposit_amount);
    assert_eq!(vault.available_balance, deposit_amount);
    assert_eq!(vault.locked_balance, 0);
    
    // Withdraw 500 USDT
    let withdraw_amount = 500000000u64;
    
    let withdraw_ix = instruction::withdraw(
        collateral_vault::id(),
        withdraw_amount,
        Withdraw {
            vault: vault_pda,
            vault_token_account: get_vault_token_account(&mut banks_client, vault_pda).await,
            user_token_account: user_usdt_account,
            user: user.pubkey(),
            token_program: token::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&payer.pubkey()),
        &[&payer, &user],
        recent_blockhash,
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    
    // Verify withdrawal
    let vault_account = banks_client.get_account(vault_pda).await.unwrap().unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_ref()).unwrap();
    
    assert_eq!(vault.total_balance, deposit_amount - withdraw_amount);
    assert_eq!(vault.available_balance, deposit_amount - withdraw_amount);
    assert_eq!(vault.locked_balance, 0);
}

#[tokio::test]
async fn test_lock_unlock_collateral() {
    let program = ProgramTest::new("collateral_vault", collateral_vault::id(), processor!(collateral_vault::entry));
    let (mut banks_client, payer, recent_blockhash) = program.start().await;
    
    let user = Keypair::new();
    let authority = Keypair::new();
    let usdt_mint = Pubkey::from_str(USDT_MINT).unwrap();
    
    // Setup vault with 1000 USDT
    let (vault_pda, _) = setup_vault(&mut banks_client, &payer, &user, &authority, usdt_mint).await;
    deposit_to_vault(&mut banks_client, &payer, &user, vault_pda, usdt_mint, 1000000000).await;
    
    // Lock 600 USDT for trading
    let lock_amount = 600000000u64;
    
    let lock_ix = instruction::lock_collateral(
        collateral_vault::id(),
        lock_amount,
        LockCollateral {
            vault: vault_pda,
            authority: authority.pubkey(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[lock_ix],
        Some(&payer.pubkey()),
        &[&payer, &authority],
        recent_blockhash,
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    
    // Verify lock
    let vault_account = banks_client.get_account(vault_pda).await.unwrap().unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_ref()).unwrap();
    
    assert_eq!(vault.total_balance, 1000000000);
    assert_eq!(vault.available_balance, 400000000);
    assert_eq!(vault.locked_balance, 600000000);
    
    // Unlock 200 USDT
    let unlock_amount = 200000000u64;
    
    let unlock_ix = instruction::unlock_collateral(
        collateral_vault::id(),
        unlock_amount,
        UnlockCollateral {
            vault: vault_pda,
            authority: authority.pubkey(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[unlock_ix],
        Some(&payer.pubkey()),
        &[&payer, &authority],
        recent_blockhash,
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    
    // Verify unlock
    let vault_account = banks_client.get_account(vault_pda).await.unwrap().unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_ref()).unwrap();
    
    assert_eq!(vault.total_balance, 1000000000);
    assert_eq!(vault.available_balance, 600000000);
    assert_eq!(vault.locked_balance, 400000000);
}

#[tokio::test]
async fn test_security_withdraw_locked_funds() {
    let program = ProgramTest::new("collateral_vault", collateral_vault::id(), processor!(collateral_vault::entry));
    let (mut banks_client, payer, recent_blockhash) = program.start().await;
    
    let user = Keypair::new();
    let authority = Keypair::new();
    let usdt_mint = Pubkey::from_str(USDT_MINT).unwrap();
    
    // Setup vault with 1000 USDT and lock 800
    let (vault_pda, _) = setup_vault(&mut banks_client, &payer, &user, &authority, usdt_mint).await;
    deposit_to_vault(&mut banks_client, &payer, &user, vault_pda, usdt_mint, 1000000000).await;
    lock_collateral(&mut banks_client, &payer, &authority, vault_pda, 800000000).await;
    
    // Attempt to withdraw 500 USDT (should fail - only 200 available)
    let withdraw_amount = 500000000u64;
    
    let withdraw_ix = instruction::withdraw(
        collateral_vault::id(),
        withdraw_amount,
        Withdraw {
            vault: vault_pda,
            vault_token_account: get_vault_token_account(&mut banks_client, vault_pda).await,
            user_token_account: create_token_account(&mut banks_client, &payer, usdt_mint, user.pubkey()).await,
            user: user.pubkey(),
            token_program: token::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[withdraw_ix],
        Some(&payer.pubkey()),
        &[&payer, &user],
        recent_blockhash,
    );
    
    let result = banks_client.process_transaction(tx).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_security_unauthorized_cpi() {
    let program = ProgramTest::new("collateral_vault", collateral_vault::id(), processor!(collateral_vault::entry));
    let (mut banks_client, payer, recent_blockhash) = program.start().await;
    
    let user = Keypair::new();
    let authority = Keypair::new();
    let unauthorized_caller = Keypair::new();
    let usdt_mint = Pubkey::from_str(USDT_MINT).unwrap();
    
    // Setup vault
    let (vault_pda, _) = setup_vault(&mut banks_client, &payer, &user, &authority, usdt_mint).await;
    deposit_to_vault(&mut banks_client, &payer, &user, vault_pda, usdt_mint, 1000000000).await;
    
    // Attempt to lock collateral with unauthorized caller
    let lock_amount = 500000000u64;
    
    let lock_ix = instruction::lock_collateral(
        collateral_vault::id(),
        lock_amount,
        LockCollateral {
            vault: vault_pda,
            authority: unauthorized_caller.pubkey(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[lock_ix],
        Some(&payer.pubkey()),
        &[&payer, &unauthorized_caller],
        recent_blockhash,
    );
    
    let result = banks_client.process_transaction(tx).await;
    assert!(result.is_err());
}

// Helper functions
async fn setup_vault(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    user: &Keypair,
    authority: &Keypair,
    usdt_mint: Pubkey,
) -> (Pubkey, u8) {
    let (vault_pda, vault_bump) = Pubkey::find_program_address(
        &[b"vault", user.pubkey().as_ref()],
        &collateral_vault::id(),
    );
    
    let (token_pda, _) = Pubkey::find_program_address(
        &[b"token", vault_pda.as_ref()],
        &collateral_vault::id(),
    );
    
    let init_ix = instruction::initialize_vault(
        collateral_vault::id(),
        vault_bump,
        InitializeVault {
            vault: vault_pda,
            vault_token_account: token_pda,
            user: user.pubkey(),
            authority: authority.pubkey(),
            usdt_mint,
            token_program: token::id(),
            system_program: system_program::id(),
            rent: rent::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[init_ix],
        Some(&payer.pubkey()),
        &[payer, user],
        banks_client.get_latest_blockhash().await.unwrap(),
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    (vault_pda, vault_bump)
}

async fn deposit_to_vault(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    user: &Keypair,
    vault_pda: Pubkey,
    usdt_mint: Pubkey,
    amount: u64,
) {
    let user_usdt_account = create_token_account(banks_client, payer, usdt_mint, user.pubkey()).await;
    mint_tokens(banks_client, payer, usdt_mint, user_usdt_account, amount).await;
    
    let deposit_ix = instruction::deposit(
        collateral_vault::id(),
        amount,
        Deposit {
            vault: vault_pda,
            vault_token_account: get_vault_token_account(banks_client, vault_pda).await,
            user_token_account: user_usdt_account,
            user: user.pubkey(),
            token_program: token::id(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[deposit_ix],
        Some(&payer.pubkey()),
        &[payer, user],
        banks_client.get_latest_blockhash().await.unwrap(),
    );
    
    banks_client.process_transaction(tx).await.unwrap();
}

async fn lock_collateral(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    authority: &Keypair,
    vault_pda: Pubkey,
    amount: u64,
) {
    let lock_ix = instruction::lock_collateral(
        collateral_vault::id(),
        amount,
        LockCollateral {
            vault: vault_pda,
            authority: authority.pubkey(),
        },
    );
    
    let tx = Transaction::new_signed_with_payer(
        &[lock_ix],
        Some(&payer.pubkey()),
        &[payer, authority],
        banks_client.get_latest_blockhash().await.unwrap(),
    );
    
    banks_client.process_transaction(tx).await.unwrap();
}

async fn create_token_account(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    mint: Pubkey,
    owner: Pubkey,
) -> Pubkey {
    let account = Keypair::new();
    let rent = banks_client.get_rent().await.unwrap();
    let space = TokenAccount::SIZE;
    let lamports = rent.minimum_balance(space);
    
    let create_ix = system_instruction::create_account(
        &payer.pubkey(),
        &account.pubkey(),
        lamports,
        space as u64,
        &token::id(),
    );
    
    let init_ix = token::instruction::initialize_account(
        &token::id(),
        &account.pubkey(),
        &mint,
        &owner,
    ).unwrap();
    
    let tx = Transaction::new_signed_with_payer(
        &[create_ix, init_ix],
        Some(&payer.pubkey()),
        &[payer, &account],
        banks_client.get_latest_blockhash().await.unwrap(),
    );
    
    banks_client.process_transaction(tx).await.unwrap();
    account.pubkey()
}

async fn mint_tokens(
    banks_client: &mut BanksClient,
    payer: &Keypair,
    mint: Pubkey,
    destination: Pubkey,
    amount: u64,
) {
    let mint_authority = payer;
    
    let mint_ix = token::instruction::mint_to(
        &token::id(),
        &mint,
        &destination,
        &mint_authority.pubkey(),
        &[],
        amount,
    ).unwrap();
    
    let tx = Transaction::new_signed_with_payer(
        &[mint_ix],
        Some(&payer.pubkey()),
        &[payer],
        banks_client.get_latest_blockhash().await.unwrap(),
    );
    
    banks_client.process_transaction(tx).await.unwrap();
}

async fn get_vault_token_account(banks_client: &mut BanksClient, vault_pda: Pubkey) -> Pubkey {
    let (token_pda, _) = Pubkey::find_program_address(
        &[b"token", vault_pda.as_ref()],
        &collateral_vault::id(),
    );
    token_pda
}