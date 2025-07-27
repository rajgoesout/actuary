use anchor_lang::{prelude::*, system_program};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

declare_id!("EiiEQwTTXftYXysyZ2VnDomcUKGDMU2SMbSrEx3Zj2dJ");

const ABOVE_SEED: &[u8] = b"above";
const BELOW_SEED: &[u8] = b"below";
const STATE_SEED: &[u8] = b"state";
const DENOM: u64 = 1_000_000_000; // 1e9 token decimals
const SECONDS_PER_DAY: u64 = 86_400;

#[account]
pub struct State {
    pub bump: u8,
    pub mint: Pubkey, // ← stores the ONLY valid ACR mint
    pub buf_bps: u16,
    pub ratchet_bps_per_day: u16, // NEW: parametrize ratchet speed (e.g., 400 = 4.00%/day)
    pub mcr: u128,
    pub virt_above: u128,
    pub virt_below: u128,
    pub last_ratchet: i64,
}

#[program]
pub mod ramm {
    use super::*;

    // ----------------------------------------------------------------------
    pub fn init(
        ctx: Context<Init>,
        buf_bps: u16,
        ratchet_bps_per_day: u16,
        mcr: u128,
    ) -> Result<()> {
        let st = &mut ctx.accounts.state;
        st.bump = ctx.bumps.state;
        st.mint = ctx.accounts.mint.key(); // NEW: persist mint
        st.buf_bps = buf_bps;
        st.ratchet_bps_per_day = ratchet_bps_per_day;
        st.mcr = mcr;
        st.virt_above = 1_000_000_000; // 1 ACR virtual
        st.virt_below = 1_000_000_000;
        st.last_ratchet = Clock::get()?.unix_timestamp;
        Ok(())
    }

    // ----------------------------------------------------------------------
    pub fn buy(ctx: Context<Trade>, lamports_in: u64) -> Result<()> {
        // NEW: hard‑check correct mint
        require!(
            ctx.accounts.mint.key() == ctx.accounts.state.mint,
            ErrorCode::WrongMint
        );

        require_keys_eq!(
            ctx.accounts.system_program.key(),
            system_program::ID,
            ErrorCode::WrongSystemProgram
        );
        require!(
            ctx.accounts.above_vault.owner == &system_program::ID,
            ErrorCode::InvalidVaultOwner
        );
        require!(
            ctx.accounts.below_vault.owner == &system_program::ID,
            ErrorCode::InvalidVaultOwner
        );

        // --- Clamp price to Book Value CEILING (BV * (1 + buffer))
        let (bv, floor, ceil) = book_value_and_bounds(&ctx)?;
        // Keep your virtual-based price as a signal, but clamp to BV bounds.
        let mut price = buy_price_virtual(&ctx.accounts.state)?;
        if price < ceil {
            price = ceil;
        }
        require!(lamports_in >= price, ErrorCode::TooLittleIn);

        // 1) move SOL → Above vault
        let transfer_instruction = anchor_lang::solana_program::system_instruction::transfer(
            &ctx.accounts.user.key(),
            &ctx.accounts.above_vault.key(),
            lamports_in,
        );
        anchor_lang::solana_program::program::invoke(
            &transfer_instruction,
            &[
                ctx.accounts.user.to_account_info(),
                ctx.accounts.above_vault.to_account_info(),
            ],
        )?;

        // 2) mint ACR to the user
        let amount_out = lamports_in * DENOM / price;
        anchor_spl::token_interface::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                anchor_spl::token_interface::MintTo {
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.user_ata.to_account_info(),
                    authority: ctx.accounts.state.to_account_info(), // PDA signs
                },
                // &[&[STATE_SEED, &[ctx.accounts.state.bump]]],
                &[&[
                    STATE_SEED,
                    ctx.accounts.mint.key().as_ref(),
                    &[ctx.accounts.state.bump],
                ]],
            ),
            amount_out,
        )?;
        ctx.accounts.state.virt_above += amount_out as u128;
        Ok(())
    }

    // ----------------------------------------------------------------------
    pub fn sell(ctx: Context<Trade>, amount_in: u64) -> Result<()> {
        require!(
            ctx.accounts.mint.key() == ctx.accounts.state.mint,
            ErrorCode::WrongMint
        ); // NEW

        // --- Owner/identity checks (vaults must be System-owned PDAs)
        require_keys_eq!(
            ctx.accounts.system_program.key(),
            system_program::ID,
            ErrorCode::WrongSystemProgram
        );
        require!(
            ctx.accounts.above_vault.owner == &system_program::ID,
            ErrorCode::InvalidVaultOwner
        );
        require!(
            ctx.accounts.below_vault.owner == &system_program::ID,
            ErrorCode::InvalidVaultOwner
        );

        // --- Clamp price to Book Value FLOOR (BV * (1 - buffer))
        let (bv, floor, _ceil) = book_value_and_bounds(&ctx)?;
        let mut price = sell_price_virtual(&ctx.accounts.state)?;
        if price > floor {
            price = floor;
        }
        let lamports_out = amount_in * price / DENOM;
        // --- Solvency check (leave 1 lamport to keep PDA alive)
        require!(
            ctx.accounts.below_vault.lamports() >= lamports_out.saturating_add(1),
            ErrorCode::InsufficientVaultBalance
        );
        // --- MCR gate: enforce post-trade Capital Pool >= MCR (i.e., MCR% >= 100%)
        // New pool balance after paying out this redemption:
        let new_liq: u128 = (ctx.accounts.above_vault.lamports() as u128)
            .saturating_add(ctx.accounts.below_vault.lamports() as u128)
            .saturating_sub(lamports_out as u128);
        require!(new_liq >= ctx.accounts.state.mcr, ErrorCode::McrBreached);

        // 1) burn ACR
        anchor_spl::token_interface::burn(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                anchor_spl::token_interface::Burn {
                    mint: ctx.accounts.mint.to_account_info(),
                    from: ctx.accounts.user_ata.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount_in,
        )?;

        // 2) transfer SOL ← Below vault
        let mint_key = ctx.accounts.mint.key();
        let seeds = &[BELOW_SEED, mint_key.as_ref()];
        let signer_seeds = &[&seeds[..]];
        let transfer_instruction = anchor_lang::solana_program::system_instruction::transfer(
            &ctx.accounts.below_vault.key(),
            &ctx.accounts.user.key(),
            lamports_out,
        );
        anchor_lang::solana_program::program::invoke_signed(
            &transfer_instruction,
            &[
                ctx.accounts.below_vault.to_account_info(),
                ctx.accounts.user.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            signer_seeds,
        )?;
        ctx.accounts.state.virt_below = ctx
            .accounts
            .state
            .virt_below
            .checked_sub(amount_in as u128)
            .ok_or(ErrorCode::Underflow)?; // NEW: safe math
        Ok(())
    }

    // ----------------------------------------------------------------------
    pub fn ratchet(ctx: Context<Ratchet>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;

        let elapsed = now - ctx.accounts.state.last_ratchet;
        if elapsed <= 0 {
            return Ok(());
        }
        // Linearized per-second drift from daily BPS (MVP-safe; can switch to exp later)
        let rate_num: u128 = (ctx.accounts.state.ratchet_bps_per_day as u128) * (elapsed as u128);
        let rate_den: u128 = (10_000u128) * (SECONDS_PER_DAY as u128);
        // Above grows by +rate, Below shrinks by -rate
        let inc_above = ctx.accounts.state.virt_above.saturating_mul(rate_num) / rate_den;
        let dec_below = ctx.accounts.state.virt_below.saturating_mul(rate_num) / rate_den;
        ctx.accounts.state.virt_above = ctx.accounts.state.virt_above.saturating_add(inc_above);
        ctx.accounts.state.virt_below = ctx.accounts.state.virt_below.saturating_sub(dec_below);
        ctx.accounts.state.last_ratchet = now;
        Ok(())
    }

    // ----------------------------------------------------------------------
    // READ-ONLY QUOTES (no state changes): emit events with price & output.
    pub fn quote_buy(ctx: Context<QuoteBuy>, lamports_in: u64) -> Result<()> {
        // clamp to BV ceiling, identical to buy()
        let (_bv, _floor, ceil) = book_value_and_bounds_from_accounts(
            &ctx.accounts.state,
            &ctx.accounts.mint,
            &ctx.accounts.above_vault,
            &ctx.accounts.below_vault,
        )?;
        let mut price = buy_price_virtual(&ctx.accounts.state)?;
        if price < ceil {
            price = ceil;
        }
        let amount_out = lamports_in.saturating_mul(DENOM) / price;
        emit!(QuoteBuyEvent {
            lamports_in,
            price,
            amount_out
        });
        Ok(())
    }

    pub fn quote_sell(ctx: Context<QuoteSell>, amount_in: u64) -> Result<()> {
        // clamp to BV floor, identical to sell()
        let (_bv, floor, _ceil) = book_value_and_bounds_from_accounts(
            &ctx.accounts.state,
            &ctx.accounts.mint,
            &ctx.accounts.above_vault,
            &ctx.accounts.below_vault,
        )?;
        let mut price = sell_price_virtual(&ctx.accounts.state)?;
        if price > floor {
            price = floor;
        }
        let lamports_out = amount_in.saturating_mul(price) / DENOM;
        emit!(QuoteSellEvent {
            amount_in,
            price,
            lamports_out
        });
        Ok(())
    }
}

// === helpers =============================================================
fn book_value_and_bounds(ctx: &Context<Trade>) -> Result<(u64, u64, u64)> {
    let liq = (ctx.accounts.above_vault.lamports() as u128)
        .saturating_add(ctx.accounts.below_vault.lamports() as u128);
    let supply = ctx.accounts.mint.supply as u128;
    require!(supply > 0, ErrorCode::TooLittleIn);
    let bv = (liq.saturating_mul(DENOM as u128) / supply) as u64; // lamports per 1 token unit (1e9)
    let floor = bv.saturating_mul(10_000 - ctx.accounts.state.buf_bps as u64) / 10_000;
    let ceil = bv.saturating_mul(10_000 + ctx.accounts.state.buf_bps as u64) / 10_000;
    Ok((bv, floor, ceil))
}
fn book_value_virtual(s: &State) -> u64 {
    ((s.virt_above + s.virt_below) / 2) as u64
}
fn buy_price_virtual(s: &State) -> Result<u64> {
    Ok(book_value_virtual(s) * (10_000 + s.buf_bps as u64) / 10_000)
}
fn sell_price_virtual(s: &State) -> Result<u64> {
    Ok(book_value_virtual(s) * (10_000 - s.buf_bps as u64) / 10_000)
}

// Same BV/bounds computation but for quote contexts (no &Context<Trade>)
fn book_value_and_bounds_from_accounts(
    state: &Account<State>,
    mint: &InterfaceAccount<Mint>,
    above_vault: &AccountInfo,
    below_vault: &AccountInfo,
) -> Result<(u64, u64, u64)> {
    let liq = (above_vault.lamports() as u128).saturating_add(below_vault.lamports() as u128);
    let supply = mint.supply as u128;
    require!(supply > 0, ErrorCode::TooLittleIn);
    let bv = (liq.saturating_mul(DENOM as u128) / supply) as u64;
    let floor = bv.saturating_mul(10_000 - state.buf_bps as u64) / 10_000;
    let ceil = bv.saturating_mul(10_000 + state.buf_bps as u64) / 10_000;
    Ok((bv, floor, ceil))
}

// === contexts (only mint check added) ======================================
#[derive(Accounts)]
pub struct Init<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    // Create State PDA (seeded by mint) so mint-authority PDA is unique per mint
    #[account(
        init,
        payer = payer,
        seeds = [STATE_SEED, mint.key().as_ref()],
        bump,
        space = 8  // anchor discriminator
            + 1    // bump
            + 32   // mint
            + 2    // buf_bps
            + 2    // ratchet_bps_per_day
            + 16   // mcr
            + 16   // virt_above
            + 16   // virt_below
            + 8    // last_ratchet
    )]
    pub state: Account<'info, State>,
    // Create System-owned zero-data vault PDAs to hold SOL
    #[account(
        init,
        payer = payer,
        seeds = [ABOVE_SEED, mint.key().as_ref()],
        bump,
        owner = system_program::ID,
        space = 0
    )]
    /// CHECK: lamport vault (System-owned)
    pub above_vault: AccountInfo<'info>,
    #[account(
        init,
        payer = payer,
        seeds = [BELOW_SEED, mint.key().as_ref()],
        bump,
        owner = system_program::ID,
        space = 0
    )]
    /// CHECK: lamport vault (System-owned)
    pub below_vault: AccountInfo<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut)]
    pub user_ata: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, seeds=[STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Account<'info, State>,
    #[account(mut, seeds=[ABOVE_SEED, mint.key().as_ref()], bump)]
    /// CHECK: lamport vault
    pub above_vault: AccountInfo<'info>,
    #[account(mut, seeds=[BELOW_SEED, mint.key().as_ref()], bump)]
    /// CHECK: lamport vault
    pub below_vault: AccountInfo<'info>,
    /// NEW: passed mint must equal state.mint
    #[account(address = state.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct Ratchet<'info> {
    #[account(mut, seeds=[STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Account<'info, State>,
    /// Ensure seed validation matches state.mint
    #[account(address = state.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(seeds=[ABOVE_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub above_vault: AccountInfo<'info>,
    #[account(seeds=[BELOW_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub below_vault: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct QuoteBuy<'info> {
    #[account(seeds=[STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Account<'info, State>,
    #[account(address = state.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(seeds=[ABOVE_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub above_vault: AccountInfo<'info>,
    #[account(seeds=[BELOW_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub below_vault: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct QuoteSell<'info> {
    #[account(seeds=[STATE_SEED, mint.key().as_ref()], bump = state.bump)]
    pub state: Account<'info, State>,
    #[account(address = state.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(seeds=[ABOVE_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub above_vault: AccountInfo<'info>,
    #[account(seeds=[BELOW_SEED, mint.key().as_ref()], bump)]
    /// CHECK:
    pub below_vault: AccountInfo<'info>,
}

// === events ================================================================
#[event]
pub struct QuoteBuyEvent {
    pub lamports_in: u64,
    pub price: u64,
    pub amount_out: u64,
}

#[event]
pub struct QuoteSellEvent {
    pub amount_in: u64,
    pub price: u64,
    pub lamports_out: u64,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Input too small")]
    TooLittleIn,
    #[msg("Attempted to trade wrong mint")]
    WrongMint, // NEW
    #[msg("Underflow")]
    Underflow,
    #[msg("Vault must be owned by System Program")]
    InvalidVaultOwner,
    #[msg("Wrong System Program account")]
    WrongSystemProgram,
    #[msg("Insufficient SOL in vault")]
    InsufficientVaultBalance,
    #[msg("Sell would breach MCR (post-trade capital < MCR)")]
    McrBreached,
}
