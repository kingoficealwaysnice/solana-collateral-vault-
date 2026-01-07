use crate::error::{Result, VaultError};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
    instruction::{Instruction, AccountMeta},
    commitment_config::CommitmentConfig,
    system_instruction,
    compute_budget::ComputeBudgetInstruction,
};
use solana_program::instruction::Instruction as ProgramInstruction;
use anchor_client::{
    Client as AnchorClient,
    Cluster,
    Program,
};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Semaphore;
use uuid::Uuid;
use tracing::{info, warn, error};

pub struct TransactionBuilder {
    rpc_client: Arc<RpcClient>,
    anchor_client: Arc<AnchorClient>,
    program: Program,
    rate_limiter: Arc<Semaphore>,
    payer: Keypair,
    program_id: Pubkey,
}

impl TransactionBuilder {
    pub fn new(
        rpc_url: &str,
        payer_keypair: Keypair,
        program_id: Pubkey,
        max_concurrent_tx: usize,
    ) -> Result<Self> {
        let rpc_client = Arc::new(RpcClient::new_with_commitment(
            rpc_url.to_string(),
            CommitmentConfig::confirmed(),
        ));
        
        let anchor_client = Arc::new(AnchorClient::new_with_options(
            Cluster::Custom(rpc_url.to_string(), rpc_url.to_string()),
            payer_keypair.clone(),
            CommitmentConfig::confirmed(),
        ));
        
        let program = anchor_client.program(program_id)?;
        
        Ok(Self {
            rpc_client,
            anchor_client,
            program,
            rate_limiter: Arc::new(Semaphore::new(max_concurrent_tx)),
            payer: payer_keypair,
            program_id,
        })
    }
    
    /// Build initialize vault transaction
    pub async fn build_initialize_vault_tx(
        &self,
        user_pubkey: Pubkey,
        authority_pubkey: Pubkey,
        usdt_mint: Pubkey,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Derive PDAs
        let (vault_pda, vault_bump) = Pubkey::find_program_address(
            &[b"vault", user_pubkey.as_ref()],
            &self.program_id,
        );
        
        let (token_pda, _) = Pubkey::find_program_address(
            &[b"token", vault_pda.as_ref()],
            &self.program_id,
        );
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::InitializeVault {
            vault: vault_pda,
            vault_token_account: token_pda,
            user: user_pubkey,
            authority: authority_pubkey,
            usdt_mint,
            token_program: spl_token::id(),
            system_program: system_program::id(),
            rent: solana_sdk::sysvar::rent::id(),
        };
        
        let data = collateral_vault::instruction::InitializeVault { bump: vault_bump };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        // Add compute budget instruction
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(300_000);
        
        let transaction = Transaction::new_signed_with_payer(
            &[compute_budget_ix, ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey: vault_pda,
            token_account_pubkey: token_pda,
            bump: vault_bump,
            estimated_compute_units: 250_000,
        })
    }
    
    /// Build deposit transaction
    pub async fn build_deposit_tx(
        &self,
        user_pubkey: Pubkey,
        vault_pubkey: Pubkey,
        amount: u64,
        user_token_account: Pubkey,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Get vault token account
        let vault_token_account = self.get_vault_token_account(vault_pubkey).await?;
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::Deposit {
            vault: vault_pubkey,
            vault_token_account,
            user_token_account,
            user: user_pubkey,
            token_program: spl_token::id(),
        };
        
        let data = collateral_vault::instruction::Deposit { amount };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey,
            token_account_pubkey: vault_token_account,
            bump: 0, // Not used for deposit
            estimated_compute_units: 100_000,
        })
    }
    
    /// Build withdraw transaction
    pub async fn build_withdraw_tx(
        &self,
        user_pubkey: Pubkey,
        vault_pubkey: Pubkey,
        amount: u64,
        user_token_account: Pubkey,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Get vault token account
        let vault_token_account = self.get_vault_token_account(vault_pubkey).await?;
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::Withdraw {
            vault: vault_pubkey,
            vault_token_account,
            user_token_account,
            user: user_pubkey,
            token_program: spl_token::id(),
        };
        
        let data = collateral_vault::instruction::Withdraw { amount };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey,
            token_account_pubkey: vault_token_account,
            bump: 0, // Not used for withdraw
            estimated_compute_units: 120_000,
        })
    }
    
    /// Build lock collateral transaction (CPI)
    pub async fn build_lock_collateral_tx(
        &self,
        vault_pubkey: Pubkey,
        amount: u64,
        authority_keypair: &Keypair,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::LockCollateral {
            vault: vault_pubkey,
            authority: authority_keypair.pubkey(),
        };
        
        let data = collateral_vault::instruction::LockCollateral { amount };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, authority_keypair],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey,
            token_account_pubkey: Pubkey::default(), // Not used
            bump: 0,
            estimated_compute_units: 80_000,
        })
    }
    
    /// Build unlock collateral transaction (CPI)
    pub async fn build_unlock_collateral_tx(
        &self,
        vault_pubkey: Pubkey,
        amount: u64,
        authority_keypair: &Keypair,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::UnlockCollateral {
            vault: vault_pubkey,
            authority: authority_keypair.pubkey(),
        };
        
        let data = collateral_vault::instruction::UnlockCollateral { amount };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, authority_keypair],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey,
            token_account_pubkey: Pubkey::default(), // Not used
            bump: 0,
            estimated_compute_units: 80_000,
        })
    }
    
    /// Build transfer collateral transaction
    pub async fn build_transfer_collateral_tx(
        &self,
        source_vault_pubkey: Pubkey,
        destination_vault_pubkey: Pubkey,
        amount: u64,
        authority_keypair: &Keypair,
    ) -> Result<BuiltTransaction> {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        
        // Get token accounts
        let source_token_account = self.get_vault_token_account(source_vault_pubkey).await?;
        let destination_token_account = self.get_vault_token_account(destination_vault_pubkey).await?;
        
        // Get recent blockhash
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        
        // Build instruction
        let accounts = collateral_vault::accounts::TransferCollateral {
            source_vault: source_vault_pubkey,
            destination_vault: destination_vault_pubkey,
            source_token_account,
            destination_token_account,
            authority: authority_keypair.pubkey(),
            token_program: spl_token::id(),
        };
        
        let data = collateral_vault::instruction::TransferCollateral { amount };
        
        let ix = Instruction {
            program_id: self.program_id,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        };
        
        let transaction = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, authority_keypair],
            recent_blockhash,
        );
        
        Ok(BuiltTransaction {
            transaction,
            vault_pubkey: source_vault_pubkey,
            token_account_pubkey: source_token_account,
            bump: 0,
            estimated_compute_units: 150_000,
        })
    }
    
    /// Get vault token account PDA
    async fn get_vault_token_account(&self, vault_pubkey: Pubkey) -> Result<Pubkey> {
        let (token_pda, _) = Pubkey::find_program_address(
            &[b"token", vault_pubkey.as_ref()],
            &self.program_id,
        );
        Ok(token_pda)
    }
    
    /// Estimate transaction cost
    pub fn estimate_transaction_cost(&self, built_tx: &BuiltTransaction) -> u64 {
        // Base fee + compute unit cost
        let base_fee = 5000; // Lamports
        let compute_cost = built_tx.estimated_compute_units * 1; // 1 lamport per compute unit
        base_fee + compute_cost
    }
}

#[derive(Debug, Clone)]
pub struct BuiltTransaction {
    pub transaction: Transaction,
    pub vault_pubkey: Pubkey,
    pub token_account_pubkey: Pubkey,
    pub bump: u8,
    pub estimated_compute_units: u32,
}

/// Transaction submission manager
pub struct TransactionSubmitter {
    rpc_client: Arc<RpcClient>,
    max_retries: u32,
    retry_delay_ms: u64,
}

impl TransactionSubmitter {
    pub fn new(rpc_client: Arc<RpcClient>, max_retries: u32, retry_delay_ms: u64) -> Self {
        Self {
            rpc_client,
            max_retries,
            retry_delay_ms,
        }
    }
    
    /// Submit transaction with retry logic
    pub async fn submit_transaction(&self, transaction: Transaction, tx_id: Uuid) -> Result<String> {
        let mut retry_count = 0;
        let mut last_error = None;
        
        loop {
            match self.send_transaction(&transaction).await {
                Ok(signature) => {
                    info!("Transaction submitted successfully: {}", signature);
                    return Ok(signature);
                }
                Err(e) => {
                    error!("Transaction submission failed (attempt {}): {}", retry_count + 1, e);
                    last_error = Some(e);
                    
                    if retry_count >= self.max_retries {
                        break;
                    }
                    
                    retry_count += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms)).await;
                }
            }
        }
        
        Err(VaultError::TransactionFailed(format!(
            "Transaction failed after {} retries: {:?}",
            self.max_retries,
            last_error
        )))
    }
    
    /// Send transaction to Solana
    async fn send_transaction(&self, transaction: &Transaction) -> Result<String> {
        let signature = self.rpc_client.send_transaction(transaction)?;
        
        // Wait for confirmation
        let confirmation = self.rpc_client.confirm_transaction(&signature)?;
        
        if confirmation {
            Ok(signature.to_string())
        } else {
            Err(VaultError::TransactionFailed("Transaction not confirmed".to_string()))
        }
    }
    
    /// Check transaction status
    pub async fn check_transaction_status(&self, signature: &str) -> Result<TransactionStatus> {
        let sig = signature.parse()
            .map_err(|_| VaultError::ValidationError("Invalid signature".to_string()))?;
        
        match self.rpc_client.get_signature_status(&sig)? {
            Some(Ok(_)) => Ok(TransactionStatus::Confirmed),
            Some(Err(e)) => Ok(TransactionStatus::Failed(e.to_string())),
            None => Ok(TransactionStatus::Pending),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TransactionStatus {
    Pending,
    Confirmed,
    Failed(String),
}