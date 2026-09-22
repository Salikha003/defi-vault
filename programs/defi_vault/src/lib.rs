use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, MintTo, Token, TokenAccount, Transfer};

// NOTE: after `anchor build`, run `anchor keys list` and replace this with the
// generated program keypair address, then update Anchor.toml [programs.devnet]
// to match.
declare_id!("VauLt1111111111111111111111111111111111");

pub const VAULT_SEED: &[u8] = b"vault";
pub const VAULT_AUTHORITY_SEED: &[u8] = b"vault_authority";
pub const USER_POSITION_SEED: &[u8] = b"user_position";

#[program]
pub mod defi_vault {
    use super::*;

    /// Admin-only: creates the vault for a given SPL mint.
    /// The vault authority PDA must be set as the mint's mint authority
    /// BEFORE calling this instruction (so the program can mint yield later).
    pub fn initialize_vault(
        ctx: Context<InitializeVault>,
        reward_rate_per_sec: u64,
        max_total_deposit: u64,
    ) -> Result<()> {
        require!(max_total_deposit > 0, VaultError::ZeroAmount);

        let vault = &mut ctx.accounts.vault_config;
        vault.admin = ctx.accounts.admin.key();
        vault.mint = ctx.accounts.mint.key();
        vault.vault_token_account = ctx.accounts.vault_token_account.key();
        vault.vault_authority_bump = ctx.bumps.vault_authority;
        vault.bump = ctx.bumps.vault_config;
        vault.reward_rate_per_sec = reward_rate_per_sec;
        vault.max_total_deposit = max_total_deposit;
        vault.total_shares = 0;
        vault.last_update_ts = Clock::get()?.unix_timestamp;

        msg!(
            "Vault initialized for mint {} with reward_rate_per_sec={}",
            vault.mint,
            reward_rate_per_sec
        );
        Ok(())
    }

    /// User operation #1: deposit SPL tokens and receive proportional shares.
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::ZeroAmount);

        let vault_balance_before = ctx.accounts.vault_token_account.amount;

        // Accrue pending yield BEFORE computing the share price so the
        // depositor doesn't dilute (or unfairly benefit from) existing
        // stakers' unclaimed rewards.
        accrue_rewards(
            &ctx.accounts.vault_config,
            &ctx.accounts.mint,
            &ctx.accounts.vault_token_account,
            &ctx.accounts.vault_authority,
            &ctx.accounts.token_program,
            ctx.bumps.vault_authority,
        )?;
        ctx.accounts.vault_token_account.reload()?;
        let vault_balance_after_accrual = ctx.accounts.vault_token_account.amount;

        let new_total_after_deposit = vault_balance_after_accrual
            .checked_add(amount)
            .ok_or(VaultError::MathOverflow)?;
        require!(
            new_total_after_deposit <= ctx.accounts.vault_config.max_total_deposit,
            VaultError::DepositCapExceeded
        );

        // Compute shares to mint using the pool-share formula:
        // shares = amount * total_shares / vault_balance  (or 1:1 on first deposit)
        let total_shares = ctx.accounts.vault_config.total_shares;
        let shares_to_mint: u64 = if total_shares == 0 || vault_balance_after_accrual == 0 {
            amount
        } else {
            (amount as u128)
                .checked_mul(total_shares as u128)
                .ok_or(VaultError::MathOverflow)?
                .checked_div(vault_balance_after_accrual as u128)
                .ok_or(VaultError::MathOverflow)?
                .try_into()
                .map_err(|_| VaultError::MathOverflow)?
        };
        require!(shares_to_mint > 0, VaultError::ZeroAmount);

        // Move tokens from user to vault.
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        token::transfer(
            CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts),
            amount,
        )?;

        let vault = &mut ctx.accounts.vault_config;
        vault.total_shares = vault
            .total_shares
            .checked_add(shares_to_mint)
            .ok_or(VaultError::MathOverflow)?;

        let position = &mut ctx.accounts.user_position;
        position.owner = ctx.accounts.user.key();
        position.vault = vault.key();
        position.shares = position
            .shares
            .checked_add(shares_to_mint)
            .ok_or(VaultError::MathOverflow)?;
        position.bump = ctx.bumps.user_position;

        // sanity: nothing was lost/created out of thin air
        require!(vault_balance_before <= vault_balance_after_accrual, VaultError::MathOverflow);

        msg!(
            "Deposit: user={} amount={} shares_minted={} total_shares={}",
            ctx.accounts.user.key(),
            amount,
            shares_to_mint,
            vault.total_shares
        );
        Ok(())
    }

    /// User operation #2: burn shares and withdraw the proportional amount
    /// of underlying tokens (principal + accrued yield).
    pub fn withdraw(ctx: Context<Withdraw>, shares_amount: u64) -> Result<()> {
        require!(shares_amount > 0, VaultError::ZeroAmount);
        require!(
            ctx.accounts.user_position.shares >= shares_amount,
            VaultError::InsufficientShares
        );

        accrue_rewards(
            &ctx.accounts.vault_config,
            &ctx.accounts.mint,
            &ctx.accounts.vault_token_account,
            &ctx.accounts.vault_authority,
            &ctx.accounts.token_program,
            ctx.bumps.vault_authority,
        )?;
        ctx.accounts.vault_token_account.reload()?;

        let vault_balance = ctx.accounts.vault_token_account.amount;
        let total_shares = ctx.accounts.vault_config.total_shares;
        require!(total_shares > 0, VaultError::VaultEmpty);

        // amount_out = shares_amount * vault_balance / total_shares
        let amount_out: u64 = (shares_amount as u128)
            .checked_mul(vault_balance as u128)
            .ok_or(VaultError::MathOverflow)?
            .checked_div(total_shares as u128)
            .ok_or(VaultError::MathOverflow)?
            .try_into()
            .map_err(|_| VaultError::MathOverflow)?;
        require!(amount_out > 0, VaultError::ZeroAmount);
        require!(amount_out <= vault_balance, VaultError::MathOverflow);

        let vault_key = ctx.accounts.vault_config.key();
        let authority_bump = ctx.bumps.vault_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[
            VAULT_AUTHORITY_SEED,
            vault_key.as_ref(),
            &[authority_bump],
        ]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.user_token_account.to_account_info(),
            authority: ctx.accounts.vault_authority.to_account_info(),
        };
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                cpi_accounts,
                signer_seeds,
            ),
            amount_out,
        )?;

        let vault = &mut ctx.accounts.vault_config;
        vault.total_shares = vault
            .total_shares
            .checked_sub(shares_amount)
            .ok_or(VaultError::MathOverflow)?;

        let position = &mut ctx.accounts.user_position;
        position.shares = position
            .shares
            .checked_sub(shares_amount)
            .ok_or(VaultError::MathOverflow)?;

        msg!(
            "Withdraw: user={} shares_burned={} amount_out={} total_shares={}",
            ctx.accounts.user.key(),
            shares_amount,
            amount_out,
            vault.total_shares
        );
        Ok(())
    }
}

/// Mints newly accrued yield into the vault's token account, proportional to
/// elapsed time. This simulates the vault "earning" interest; in a mainnet
/// version this would instead come from a real yield source (lending,
/// LP fees, etc.) rather than minting.
fn accrue_rewards<'info>(
    vault_config: &Account<'info, VaultConfig>,
    mint: &Account<'info, Mint>,
    vault_token_account: &Account<'info, TokenAccount>,
    vault_authority: &UncheckedAccount<'info>,
    token_program: &Program<'info, Token>,
    authority_bump: u8,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let elapsed = now.saturating_sub(vault_config.last_update_ts);

    if elapsed <= 0 || vault_config.total_shares == 0 || vault_config.reward_rate_per_sec == 0 {
        return Ok(());
    }

    let reward_amount = (elapsed as u128)
        .checked_mul(vault_config.reward_rate_per_sec as u128)
        .ok_or(VaultError::MathOverflow)?
        .try_into()
        .map_err(|_| VaultError::MathOverflow)?;

    if reward_amount == 0 {
        return Ok(());
    }

    let vault_key = vault_config.key();
    let signer_seeds: &[&[&[u8]]] = &[&[
        VAULT_AUTHORITY_SEED,
        vault_key.as_ref(),
        &[authority_bump],
    ]];

    let cpi_accounts = MintTo {
        mint: mint.to_account_info(),
        to: vault_token_account.to_account_info(),
        authority: vault_authority.to_account_info(),
    };
    token::mint_to(
        CpiContext::new_with_signer(token_program.to_account_info(), cpi_accounts, signer_seeds),
        reward_amount,
    )?;

    Ok(())
}

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = admin,
        space = 8 + VaultConfig::INIT_SPACE,
        seeds = [VAULT_SEED, mint.key().as_ref()],
        bump
    )]
    pub vault_config: Account<'info, VaultConfig>,

    /// PDA that owns the vault token account and is the mint's mint authority.
    /// CHECK: PDA, not read as data — only used as a signing authority.
    #[account(
        seeds = [VAULT_AUTHORITY_SEED, vault_config.key().as_ref()],
        bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        init,
        payer = admin,
        token::mint = mint,
        token::authority = vault_authority,
        seeds = [b"vault_tokens", vault_config.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [VAULT_SEED, mint.key().as_ref()],
        bump = vault_config.bump,
        has_one = mint,
    )]
    pub vault_config: Account<'info, VaultConfig>,

    pub mint: Account<'info, Mint>,

    /// CHECK: PDA signer only, verified via seeds.
    #[account(
        seeds = [VAULT_AUTHORITY_SEED, vault_config.key().as_ref()],
        bump = vault_config.vault_authority_bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        address = vault_config.vault_token_account @ VaultError::InvalidVaultTokenAccount
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_token_account.mint == mint.key() @ VaultError::MintMismatch,
        constraint = user_token_account.owner == user.key() @ VaultError::Unauthorized,
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = user,
        space = 8 + UserPosition::INIT_SPACE,
        seeds = [USER_POSITION_SEED, vault_config.key().as_ref(), user.key().as_ref()],
        bump
    )]
    pub user_position: Account<'info, UserPosition>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [VAULT_SEED, mint.key().as_ref()],
        bump = vault_config.bump,
        has_one = mint,
    )]
    pub vault_config: Account<'info, VaultConfig>,

    pub mint: Account<'info, Mint>,

    /// CHECK: PDA signer only, verified via seeds.
    #[account(
        seeds = [VAULT_AUTHORITY_SEED, vault_config.key().as_ref()],
        bump = vault_config.vault_authority_bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        address = vault_config.vault_token_account @ VaultError::InvalidVaultTokenAccount
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_token_account.mint == mint.key() @ VaultError::MintMismatch,
        constraint = user_token_account.owner == user.key() @ VaultError::Unauthorized,
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [USER_POSITION_SEED, vault_config.key().as_ref(), user.key().as_ref()],
        bump = user_position.bump,
        constraint = user_position.owner == user.key() @ VaultError::Unauthorized,
    )]
    pub user_position: Account<'info, UserPosition>,

    pub token_program: Program<'info, Token>,
}

#[account]
#[derive(InitSpace)]
pub struct VaultConfig {
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub vault_token_account: Pubkey,
    pub reward_rate_per_sec: u64,
    pub max_total_deposit: u64,
    pub total_shares: u64,
    pub last_update_ts: i64,
    pub vault_authority_bump: u8,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct UserPosition {
    pub owner: Pubkey,
    pub vault: Pubkey,
    pub shares: u64,
    pub bump: u8,
}

#[error_code]
pub enum VaultError {
    #[msg("Amount must be greater than zero.")]
    ZeroAmount,
    #[msg("Arithmetic overflow or invalid conversion.")]
    MathOverflow,
    #[msg("User does not have enough shares.")]
    InsufficientShares,
    #[msg("Vault has no shares outstanding.")]
    VaultEmpty,
    #[msg("Deposit would exceed the vault's maximum total deposit cap.")]
    DepositCapExceeded,
    #[msg("Token account mint does not match vault mint.")]
    MintMismatch,
    #[msg("Signer is not authorized for this account.")]
    Unauthorized,
    #[msg("Provided vault token account does not match the vault config.")]
    InvalidVaultTokenAccount,
}
