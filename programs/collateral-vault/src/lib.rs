use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Mint, Transfer};
use std::str::FromStr;

declare_id!("CVault111111111111111111111111111111111111111");

#[program]
pub mod collateral_vault {
    use super::*;

    /// Initialize a new collateral vault for a user
    /// 
    /// Security considerations:
    /// - Only one vault per user (enforced by PDA seed)
    /// - Vault PDA derived from user pubkey + constant seed
    /// - Token account owned by vault PDA (not user)
    /// - Initial balances set to zero
    pub fn initialize_vault(ctx: Context<InitializeVault>, bump: u8) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;
        
        vault.user = ctx.accounts.user.key();
        vault.token_account = ctx.accounts.vault_token_account.key();
        vault.bump = bump;
        vault.total_balance = 0;
        vault.locked_balance = 0;
        vault.available_balance = 0;
        vault.last_updated = clock.unix_timestamp;
        vault.is_active = true;
        vault.authority = ctx.accounts.authority.key();
        
        emit!(VaultInitialized {
            user: vault.user,
            vault: vault.key(),
            token_account: vault.token_account,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Deposit USDT into user's vault
    /// 
    /// Security checks:
    /// - Vault must be active
    /// - User must sign
    /// - Overflow protection on balance updates
    /// - SPL token transfer verification
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::InvalidAmount);
        require!(ctx.accounts.vault.is_active, VaultError::VaultInactive);
        
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;
        
        // Update vault balances with overflow protection
        vault.total_balance = vault.total_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        vault.available_balance = vault.available_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        vault.last_updated = clock.unix_timestamp;
        
        // Perform SPL token transfer from user to vault
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        
        token::transfer(cpi_ctx, amount)?;
        
        emit!(DepositEvent {
            user: vault.user,
            vault: vault.key(),
            amount,
            new_total_balance: vault.total_balance,
            new_available_balance: vault.available_balance,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Withdraw available balance from vault
    /// 
    /// Critical security checks:
    /// - Only vault owner can withdraw
    /// - Cannot withdraw locked funds
    /// - Amount must be <= available_balance
    /// - Vault remains solvent after withdrawal
    pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::InvalidAmount);
        require!(ctx.accounts.vault.is_active, VaultError::VaultInactive);
        
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;
        
        // Ensure sufficient available balance
        require!(vault.available_balance >= amount, VaultError::InsufficientAvailableBalance);
        
        // Update balances with underflow protection
        vault.total_balance = vault.total_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        vault.available_balance = vault.available_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        vault.last_updated = clock.unix_timestamp;
        
        // Transfer tokens from vault to user
        let vault_key = vault.key();
        let bump = vault.bump;
        let signer_seeds = &[
            b"vault",
            vault.user.as_ref(),
            &[bump],
        ];
        let signer = &[&signer_seeds[..]];
        
        let cpi_accounts = Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.user_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        
        token::transfer(cpi_ctx, amount)?;
        
        emit!(WithdrawEvent {
            user: vault.user,
            vault: vault.key(),
            amount,
            new_total_balance: vault.total_balance,
            new_available_balance: vault.available_balance,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Lock collateral for trading positions (CPI-only)
    /// 
    /// Security: Only authorized trading program can call this
    /// Prevents double-spending of collateral
    pub fn lock_collateral(ctx: Context<LockCollateral>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::InvalidAmount);
        require!(ctx.accounts.vault.is_active, VaultError::VaultInactive);
        
        // Verify caller is authorized trading program
        require!(ctx.accounts.authority.key() == ctx.accounts.vault.authority, 
                 VaultError::UnauthorizedCaller);
        
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;
        
        // Ensure sufficient available balance
        require!(vault.available_balance >= amount, VaultError::InsufficientAvailableBalance);
        
        // Atomically update balances
        vault.available_balance = vault.available_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        vault.locked_balance = vault.locked_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        vault.last_updated = clock.unix_timestamp;
        
        emit!(CollateralLocked {
            user: vault.user,
            vault: vault.key(),
            amount,
            new_available_balance: vault.available_balance,
            new_locked_balance: vault.locked_balance,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Unlock collateral when positions are closed (CPI-only)
    /// 
    /// Security: Only authorized trading program can unlock
    /// Amount must not exceed locked balance
    pub fn unlock_collateral(ctx: Context<UnlockCollateral>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::InvalidAmount);
        require!(ctx.accounts.vault.is_active, VaultError::VaultInactive);
        
        // Verify caller is authorized trading program
        require!(ctx.accounts.authority.key() == ctx.accounts.vault.authority, 
                 VaultError::UnauthorizedCaller);
        
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;
        
        // Ensure sufficient locked balance
        require!(vault.locked_balance >= amount, VaultError::InsufficientLockedBalance);
        
        // Atomically update balances
        vault.locked_balance = vault.locked_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        vault.available_balance = vault.available_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        vault.last_updated = clock.unix_timestamp;
        
        emit!(CollateralUnlocked {
            user: vault.user,
            vault: vault.key(),
            amount,
            new_available_balance: vault.available_balance,
            new_locked_balance: vault.locked_balance,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Transfer collateral between vaults (authorized internal settlement)
    /// 
    /// Security: Only authorized programs can transfer
    /// Both vaults must be active
    /// Source must have sufficient locked balance
    pub fn transfer_collateral(ctx: Context<TransferCollateral>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::InvalidAmount);
        require!(ctx.accounts.source_vault.is_active, VaultError::VaultInactive);
        require!(ctx.accounts.destination_vault.is_active, VaultError::VaultInactive);
        
        // Verify caller is authorized
        require!(ctx.accounts.authority.key() == ctx.accounts.source_vault.authority, 
                 VaultError::UnauthorizedCaller);
        
        let source_vault = &mut ctx.accounts.source_vault;
        let destination_vault = &mut ctx.accounts.destination_vault;
        let clock = Clock::get()?;
        
        // Ensure source has sufficient locked balance for transfer
        require!(source_vault.locked_balance >= amount, VaultError::InsufficientLockedBalance);
        
        // Update source vault (reduce locked, don't affect available)
        source_vault.locked_balance = source_vault.locked_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        source_vault.total_balance = source_vault.total_balance.checked_sub(amount)
            .ok_or(VaultError::Underflow)?;
        source_vault.last_updated = clock.unix_timestamp;
        
        // Update destination vault (increase available and total)
        destination_vault.total_balance = destination_vault.total_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        destination_vault.available_balance = destination_vault.available_balance.checked_add(amount)
            .ok_or(VaultError::Overflow)?;
        destination_vault.last_updated = clock.unix_timestamp;
        
        // Perform actual token transfer
        let source_key = source_vault.key();
        let source_bump = source_vault.bump;
        let source_seeds = &[
            b"vault",
            source_vault.user.as_ref(),
            &[source_bump],
        ];
        let signer = &[&source_seeds[..]];
        
        let cpi_accounts = Transfer {
            from: ctx.accounts.source_token_account.to_account_info(),
            to: ctx.accounts.destination_token_account.to_account_info(),
            authority: source_vault.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        
        token::transfer(cpi_ctx, amount)?;
        
        emit!(CollateralTransferred {
            source_user: source_vault.user,
            destination_user: destination_vault.user,
            source_vault: source_vault.key(),
            destination_vault: destination_vault.key(),
            amount,
            timestamp: clock.unix_timestamp,
        });
        
        Ok(())
    }
}

#[account]
#[derive(Debug)]
pub struct Vault {
    pub user: Pubkey,                    // Owner of the vault
    pub token_account: Pubkey,          // Associated token account
    pub bump: u8,                       // PDA bump seed
    pub total_balance: u64,            // Total USDT balance
    pub locked_balance: u64,           // Locked for positions
    pub available_balance: u64,        // Available for withdrawal
    pub last_updated: i64,             // Last update timestamp
    pub is_active: bool,              // Vault status
    pub authority: Pubkey,             // Authorized programs for CPI calls
}

impl Vault {
    pub const SIZE: usize = 32 + 32 + 1 + 8 + 8 + 8 + 8 + 1 + 32 + 32; // Account size
    
    /// Critical invariant: available_balance + locked_balance == total_balance
    pub fn validate_invariant(&self) -> Result<()> {
        let calculated_total = self.available_balance.checked_add(self.locked_balance)
            .ok_or(VaultError::Overflow)?;
        require!(calculated_total == self.total_balance, VaultError::InvariantViolated);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(
        init,
        payer = user,
        space = Vault::SIZE,
        seeds = [b"vault", user.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, Vault>,
    
    #[account(
        init,
        payer = user,
        token::mint = usdt_mint,
        token::authority = vault,
        seeds = [b"token", vault.key().as_ref()],
        bump,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub user: Signer<'info>,
    
    /// CHECK: Authority for CPI calls (trading program)
    pub authority: AccountInfo<'info>,
    
    pub usdt_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(
        mut,
        has_one = user,
        constraint = vault.is_active @ VaultError::VaultInactive,
    )]
    pub vault: Account<'info, Vault>,
    
    #[account(
        mut,
        constraint = vault_token_account.key() == vault.token_account,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        constraint = user_token_account.owner == user.key(),
        constraint = user_token_account.mint == vault_token_account.mint,
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub user: Signer<'info>,
    
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(
        mut,
        has_one = user,
        constraint = vault.is_active @ VaultError::VaultInactive,
    )]
    pub vault: Account<'info, Vault>,
    
    #[account(
        mut,
        constraint = vault_token_account.key() == vault.token_account,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        constraint = user_token_account.owner == user.key(),
        constraint = user_token_account.mint == vault_token_account.mint,
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub user: Signer<'info>,
    
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct LockCollateral<'info> {
    #[account(
        mut,
        constraint = vault.is_active @ VaultError::VaultInactive,
    )]
    pub vault: Account<'info, Vault>,
    
    /// CHECK: Authority must match vault.authority for CPI calls
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UnlockCollateral<'info> {
    #[account(
        mut,
        constraint = vault.is_active @ VaultError::VaultInactive,
    )]
    pub vault: Account<'info, Vault>,
    
    /// CHECK: Authority must match vault.authority for CPI calls
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct TransferCollateral<'info> {
    #[account(
        mut,
        constraint = source_vault.is_active @ VaultError::VaultInactive,
    )]
    pub source_vault: Account<'info, Vault>,
    
    #[account(
        mut,
        constraint = destination_vault.is_active @ VaultError::VaultInactive,
    )]
    pub destination_vault: Account<'info, Vault>,
    
    #[account(
        mut,
        constraint = source_token_account.key() == source_vault.token_account,
    )]
    pub source_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        constraint = destination_token_account.key() == destination_vault.token_account,
        constraint = destination_token_account.mint == source_token_account.mint,
    )]
    pub destination_token_account: Account<'info, TokenAccount>,
    
    /// CHECK: Authority must match source_vault.authority for transfers
    pub authority: Signer<'info>,
    
    pub token_program: Program<'info, Token>,
}

#[error_code]
pub enum VaultError {
    #[msg("Vault is inactive")]
    VaultInactive,
    #[msg("Insufficient available balance")]
    InsufficientAvailableBalance,
    #[msg("Insufficient locked balance")]
    InsufficientLockedBalance,
    #[msg("Invalid amount")]
    InvalidAmount,
    #[msg("Unauthorized caller - only authorized programs can call this function")]
    UnauthorizedCaller,
    #[msg("Math overflow occurred")]
    Overflow,
    #[msg("Math underflow occurred")]
    Underflow,
    #[msg("Vault invariant violated - balances don't add up")]
    InvariantViolated,
}

#[event]
pub struct VaultInitialized {
    pub user: Pubkey,
    pub vault: Pubkey,
    pub token_account: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DepositEvent {
    pub user: Pubkey,
    pub vault: Pubkey,
    pub amount: u64,
    pub new_total_balance: u64,
    pub new_available_balance: u64,
    pub timestamp: i64,
}

#[event]
pub struct WithdrawEvent {
    pub user: Pubkey,
    pub vault: Pubkey,
    pub amount: u64,
    pub new_total_balance: u64,
    pub new_available_balance: u64,
    pub timestamp: i64,
}

#[event]
pub struct CollateralLocked {
    pub user: Pubkey,
    pub vault: Pubkey,
    pub amount: u64,
    pub new_available_balance: u64,
    pub new_locked_balance: u64,
    pub timestamp: i64,
}

#[event]
pub struct CollateralUnlocked {
    pub user: Pubkey,
    pub vault: Pubkey,
    pub amount: u64,
    pub new_available_balance: u64,
    pub new_locked_balance: u64,
    pub timestamp: i64,
}

#[event]
pub struct CollateralTransferred {
    pub source_user: Pubkey,
    pub destination_user: Pubkey,
    pub source_vault: Pubkey,
    pub destination_vault: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}