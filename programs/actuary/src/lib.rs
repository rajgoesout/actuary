use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Mint, Token, TokenAccount, Transfer},
};

declare_id!("6dktB5XDeCN2Gw91Ux8NJSS3L6b7htwBUhGkS7TLC4bJ");

#[program]
pub mod actuary {
    use super::*;

    // ─── Initialize: set USDC mint & admin ───────────────────────────────
    pub fn initialize(ctx: Context<Initialize>, admin: Pubkey) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.admin      = admin;
        cfg.usdc_mint  = ctx.accounts.usdc_mint.key();
        cfg.bump       = ctx.bumps.config;
        Ok(())
    }

    // ─── Stake: lock USDC into the pool vault ────────────────────────────
    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        // Transfer USDC from user → pool vault via CPI :contentReference[oaicite:1]{index=1}
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from:      ctx.accounts.staker_ata.to_account_info(),
                    to:        ctx.accounts.pool_vault.to_account_info(),
                    authority: ctx.accounts.staker.to_account_info(),
                },
            ),
            amount,
        )?;

        // Record stake in PDA
        let rec = &mut ctx.accounts.stake_rec;
        rec.staker = ctx.accounts.staker.key();
        rec.amount = rec.amount.checked_add(amount).unwrap();
        rec.bump = ctx.bumps.stake_rec;
        Ok(())
    }

    // ─── BuyCover: pay premium, get coverage ──────────────────────────
    pub fn buy_cover(ctx: Context<BuyCover>, cover_id: u64, amount: u64, premium: u64, duration: i64) -> Result<()> {
        // Validate cover type exists
        let cover_type = &ctx.accounts.cover_type;
        // Transfer premium from claimant to pool vault
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from:      ctx.accounts.claimant_ata.to_account_info(),
                    to:        ctx.accounts.pool_vault.to_account_info(),
                    authority: ctx.accounts.claimant.to_account_info(),
                },
            ),
            premium,
        )?;

        // Create Cover account
        let cover = &mut ctx.accounts.cover;
        cover.claimant = ctx.accounts.claimant.key();
        cover.cover_id = cover_id;
        cover.cover_name = cover_type.name;
        cover.amount = amount;
        cover.premium_paid = premium;
        cover.start_ts = Clock::get()?.unix_timestamp;
        cover.duration = duration;
        cover.active = true;
        cover.bump = ctx.bumps.cover;

        // Track cover_id in UserCovers
        let user_covers = &mut ctx.accounts.user_covers;
        if !user_covers.cover_ids.contains(&cover_id) {
            user_covers.cover_ids.push(cover_id);
        }
        Ok(())
    }

    // ─── CreateClaim: only if valid Cover ─────────────────────────────
    pub fn create_claim(ctx: Context<CreateClaim>, cover_id: u64) -> Result<()> {
        let cover = &mut ctx.accounts.cover;
        let now = Clock::get()?.unix_timestamp;
        require!(cover.active, InsuranceError::NoActiveCover);
        require!(now >= cover.start_ts, InsuranceError::CoverNotStarted);
        require!(now <= cover.start_ts + cover.duration, InsuranceError::CoverExpired);

        let claim = &mut ctx.accounts.claim;
        claim.claimant = ctx.accounts.claimant.key();
        claim.yes      = 0;
        claim.no       = 0;
        claim.bump     = ctx.bumps.claim;
        Ok(())
    }

    // ─── Vote: yes/no weighted by staked USDC ────────────────────────────
    pub fn vote(ctx: Context<Vote>, approve: bool) -> Result<()> {
        // Ensure staker has a record
        let stake_rec = &ctx.accounts.stake_rec;
        require!(stake_rec.amount > 0, InsuranceError::NoStake);

        // Prevent double-vote
        let vr = &mut ctx.accounts.vote_rec;
        require!(!vr.voted, InsuranceError::AlreadyVoted);

        // Tally vote
        let claim = &mut ctx.accounts.claim;
        if approve {
            claim.yes = claim.yes.checked_add(stake_rec.amount).unwrap();
        } else {
            claim.no  = claim.no.checked_add(stake_rec.amount).unwrap();
        }
        vr.voted   = true;
        vr.bump    = ctx.bumps.vote_rec;
        Ok(())
    }

    // ─── Resolve: payout limited to cover amount ──────────────────────
    pub fn resolve(ctx: Context<Resolve>, cover_id: u64) -> Result<()> {
        let claim = &ctx.accounts.claim;
        let cover = &mut ctx.accounts.cover;
        require!(claim.yes > claim.no, InsuranceError::ClaimDenied);
        require!(cover.active, InsuranceError::NoActiveCover);
        // Deactivate cover after claim
        cover.active = false;
        // Transfer USDC from vault → claimant ATA, up to cover amount
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from:      ctx.accounts.pool_vault.to_account_info(),
                    to:        ctx.accounts.claimant_ata.to_account_info(),
                    authority: ctx.accounts.pool_authority.clone(),
                },
            ),
            cover.amount,
        )?;
        Ok(())
    }

    pub fn add_cover_type(ctx: Context<AddCoverType>, cover_id: u64, name: [u8; 32]) -> Result<()> {
        let cover_type = &mut ctx.accounts.cover_type;
        cover_type.cover_id = cover_id;
        cover_type.name = name;
        cover_type.bump = ctx.bumps.cover_type;
        Ok(())
    }

    pub fn remove_cover_type(_ctx: Context<RemoveCoverType>) -> Result<()> {
        // Just close the account
        Ok(())
    }
}

// ─── On‑chain State ─────────────────────────────────────────────────────────

#[account]
pub struct Config {
    pub admin:     Pubkey,
    pub usdc_mint: Pubkey,
    pub bump:      u8,
}

#[account]
pub struct StakeRec {
    pub staker: Pubkey,
    pub amount: u64,
    pub bump:   u8,
}

#[account]
pub struct Claim {
    pub claimant: Pubkey,
    pub yes:      u64,
    pub no:       u64,
    pub bump:     u8,
}

#[account]
pub struct VoteRec {
    pub voted: bool,
    pub bump:  u8,
}

// ─── Add Cover Account ───────────────────────────────────────────────
#[account]
pub struct Cover {
    pub claimant: Pubkey,
    pub cover_id: u64,        // Unique cover ID for this user
    pub cover_name: [u8; 32], // Name of the cover type
    pub amount: u64,         // Coverage amount
    pub premium_paid: u64,   // USDC paid as premium
    pub start_ts: i64,       // Coverage start timestamp
    pub duration: i64,       // Coverage duration (seconds)
    pub active: bool,
    pub bump: u8,
}

/// Tracks all cover_ids for a user for easy querying
#[account]
pub struct UserCovers {
    pub user: Pubkey,
    pub cover_ids: Vec<u64>, // All cover_ids for this user
    pub bump: u8,
}

// ─── Admin-defined Cover Types ───────────────────────────────────────
#[account]
pub struct CoverType {
    pub cover_id: u64,      // Unique cover type ID
    pub name: [u8; 32],     // Name of the cover (fixed size, UTF-8, null-padded)
    pub bump: u8,
}

// ─── Add/Remove CoverType Admin Instructions ─────────────────────────
#[derive(Accounts)]
#[instruction(cover_id: u64)]
pub struct AddCoverType<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        init,
        seeds = [b"cover_type"[..].as_ref(), &cover_id.to_le_bytes()],
        bump,
        payer = admin,
        space = 8 + 32 + 32 + 1
    )]
    pub cover_type: Account<'info, CoverType>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(cover_id: u64)]
pub struct RemoveCoverType<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        mut,
        close = admin,
        seeds = [b"cover_type", &cover_id.to_le_bytes()],
        bump = cover_type.bump,
    )]
    pub cover_type: Account<'info, CoverType>,

    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

// ─── Instruction Contexts ───────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(admin: Pubkey)]
pub struct Initialize<'info> {
    #[account(
        init,
        seeds = [b"config"],
        bump,
        payer = payer,
        space = 8 + 32 + 32 + 1
    )]
    pub config:     Account<'info, Config>,
    pub usdc_mint:  Account<'info, Mint>,
    #[account(mut)]
    pub payer:      Signer<'info>,
    pub system_program:            Program<'info, System>,
    pub rent:                      Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(mut)]
    pub config:    Account<'info, Config>,

    #[account(mut)]
    pub staker:    Signer<'info>,

    #[account(
        init_if_needed,
        seeds = [b"stake", staker.key().as_ref()],
        bump,
        payer = staker,
        space = 8 + 32 + 8 + 1
    )]
    pub stake_rec: Account<'info, StakeRec>,

    #[account(mut)]
    pub staker_ata: Account<'info, TokenAccount>,

    pub usdc_mint: Account<'info, Mint>,

    /// Pool vault ATA for USDC (ATA seeds = [usdc_mint, pool_authority])
    #[account(
        init_if_needed,
        associated_token::mint = usdc_mint,
        associated_token::authority = pool_authority,
        payer = staker
    )]
    pub pool_vault: Account<'info, TokenAccount>,

    /// CHECK: This is a PDA authority, not a real account. No data is read or written.
    #[account(seeds = [b"config"], bump = config.bump)]
    pub pool_authority: AccountInfo<'info>,

    pub token_program:            Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program:           Program<'info, System>,
    pub rent:                     Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(cover_id: u64)]
pub struct CreateClaim<'info> {
    #[account(mut)]
    pub config:    Account<'info, Config>,

    #[account(mut)]
    pub claimant:  Signer<'info>,

    #[account(
        mut,
        seeds = [b"cover", claimant.key().as_ref(), &cover_id.to_le_bytes()],
        bump = cover.bump,
        has_one = claimant
    )]
    pub cover:     Account<'info, Cover>,

    #[account(
        init,
        seeds = [b"claim", claimant.key().as_ref(), &cover_id.to_le_bytes()],
        bump,
        payer = claimant,
        space = 8 + 32 + 8 + 8 + 1
    )]
    pub claim:     Account<'info, Claim>,

    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Vote<'info> {
    #[account(mut)]
    pub claim:     Account<'info, Claim>,

    #[account(mut)]
    pub stake_rec: Account<'info, StakeRec>,

    #[account(
        init_if_needed,
        seeds = [b"vote", claim.key().as_ref(), stake_rec.staker.as_ref()],
        bump,
        payer = stake_rec,
        space = 8 + 1 + 1
    )]
    pub vote_rec:  Account<'info, VoteRec>,

    pub system_program: Program<'info, System>,
    pub rent:           Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(cover_id: u64)]
pub struct Resolve<'info> {
    #[account(mut, has_one = admin)]
    pub config: Account<'info, Config>,

    pub admin: Signer<'info>,

    #[account(mut)]
    pub claim:     Account<'info, Claim>,

    #[account(mut,
        seeds = [b"cover", claim.claimant.as_ref(), &cover_id.to_le_bytes()],
        bump = cover.bump,
    )]
    pub cover:     Account<'info, Cover>,

    #[account(mut)]
    pub pool_vault: Account<'info, TokenAccount>,

    /// Re-use the same PDA authority
    /// CHECK: This is a PDA authority, not a real account. No data is read or written.
    #[account(seeds = [b"config"], bump = config.bump)]
    pub pool_authority: AccountInfo<'info>,

    #[account(mut)]
    pub claimant_ata: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
#[instruction(cover_id: u64)]
pub struct BuyCover<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,

    #[account(mut)]
    pub claimant: Signer<'info>,

    #[account(
        mut,
        seeds = [b"cover_type", &cover_id.to_le_bytes()],
        bump = cover_type.bump,
    )]
    pub cover_type: Account<'info, CoverType>,

    #[account(
        init,
        seeds = [b"cover", claimant.key().as_ref(), &cover_id.to_le_bytes()],
        bump,
        payer = claimant,
        space = 8 + 32 + 8 + 8 + 8 + 8 + 1 + 1 + 8 // extra for cover_id
    )]
    pub cover: Account<'info, Cover>,

    #[account(
        init_if_needed,
        seeds = [b"user_covers", claimant.key().as_ref()],
        bump,
        payer = claimant,
        space = 8 + 32 + (8 * 32) + 1 // 32 covers max, adjust as needed
    )]
    pub user_covers: Account<'info, UserCovers>,

    #[account(mut)]
    pub claimant_ata: Account<'info, TokenAccount>,

    #[account(mut)]
    pub pool_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[error_code]
pub enum InsuranceError {
    #[msg("No stake found")]
    NoStake,
    #[msg("Already voted")]
    AlreadyVoted,
    #[msg("Claim not approved")]
    ClaimDenied,
    #[msg("No active cover")]
    NoActiveCover,
    #[msg("Cover not started yet")]
    CoverNotStarted,
    #[msg("Cover expired")]
    CoverExpired,
}

// Querying covers:
// - To get all covers for a user: fetch UserCovers PDA [b"user_covers", user_pubkey]
//   and then fetch each Cover PDA [b"cover", user_pubkey, &cover_id.to_le_bytes()] for each cover_id in cover_ids.
// - To filter by cover size: fetch all covers for the user, then filter by amount in client code.