//! Veritas — on-chain plausibility commitment & challenge protocol.
//!
//! An *optimistic* verification game. A submitter commits a physics-scored
//! claim (only its hash + headline figures go on-chain; the full claim stays
//! off-chain and cheap) and escrows a bond. Anyone can challenge it by staking
//! while the challenge window is open. A challenged claim is settled by the
//! configured resolver, who re-runs the deterministic physics engine on the
//! committed inputs and signs the score it derives.
//!
//! MVP honesty: `resolve` trusts the configured resolver's re-derived score.
//! The submitter's own score is recorded but never decides a dispute. Replacing
//! the single resolver with verifiable compute or a committee of independent
//! re-runners is the core post-hackathon research problem.
//!
//! Funds never stay locked: an unchallenged bond can be withdrawn once the
//! challenge window closes, a confirmed claim gets its bond back at resolution,
//! and anyone can refund both sides of a challenge the resolver leaves past its
//! deadline.
//!
//! Built against Anchor 0.30.x.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("DypSeezrbcEhDSJNganfpjjkQXp1NBAHpvDAQrQBHLEW");

use crate::program::Veritas;

/// Resolver scores at or below this (basis points, 0..10000) are implausible.
/// 0.50 -> 5000 bps.
pub const IMPLAUSIBLE_THRESHOLD_BPS: u16 = 5000;

/// Minimum bond a submitter must stake (lamports). 0.05 SOL.
pub const MIN_BOND_LAMPORTS: u64 = 50_000_000;

#[program]
pub mod veritas {
    use super::*;

    /// One-time setup, callable only by the program's upgrade authority: names
    /// the resolver and sets the challenge and resolution windows.
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        resolver: Pubkey,
        challenge_window_secs: i64,
        resolve_window_secs: i64,
    ) -> Result<()> {
        require_gt!(challenge_window_secs, 0, VeritasError::BadWindow);
        require_gt!(resolve_window_secs, 0, VeritasError::BadWindow);

        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.resolver = resolver;
        config.challenge_window_secs = challenge_window_secs;
        config.resolve_window_secs = resolve_window_secs;
        config.bump = ctx.bumps.config;
        Ok(())
    }

    /// Rotate the resolver. Callable only by the program's upgrade authority.
    pub fn set_resolver(ctx: Context<SetResolver>, new_resolver: Pubkey) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let previous = config.resolver;
        config.resolver = new_resolver;

        emit!(ResolverChanged {
            config: config.key(),
            previous,
            resolver: new_resolver,
        });
        Ok(())
    }

    /// Commit a physics-scored claim. Escrows `bond` in the claim PDA.
    ///
    /// `integrity_score_bps` is the submitter's own engine score. It is recorded
    /// for reference; a dispute is settled by the resolver's score instead.
    pub fn submit_claim(
        ctx: Context<SubmitClaim>,
        inputs_hash: [u8; 32],
        model_version: u32,
        claimed_co2_kg: u64,
        integrity_score_bps: u16,
        bond: u64,
    ) -> Result<()> {
        require!(bond >= MIN_BOND_LAMPORTS, VeritasError::BondTooLow);
        require!(integrity_score_bps <= 10_000, VeritasError::BadScore);

        // Move the bond from submitter into the claim PDA (escrow).
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.submitter.to_account_info(),
                    to: ctx.accounts.claim.to_account_info(),
                },
            ),
            bond,
        )?;

        let claim = &mut ctx.accounts.claim;
        claim.submitter = ctx.accounts.submitter.key();
        claim.inputs_hash = inputs_hash;
        claim.model_version = model_version;
        claim.claimed_co2_kg = claimed_co2_kg;
        claim.integrity_score_bps = integrity_score_bps;
        claim.bond = bond;
        claim.status = ClaimStatus::Pending;
        claim.challenger = None;
        claim.created_at = Clock::get()?.unix_timestamp;
        claim.bump = ctx.bumps.claim;
        claim.resolved_score_bps = None;

        emit!(ClaimSubmitted {
            claim: claim.key(),
            submitter: claim.submitter,
            inputs_hash,
            integrity_score_bps,
        });
        Ok(())
    }

    /// Dispute a pending claim while its challenge window is open. Escrows
    /// `stake` in the challenge PDA.
    /// `recomputed_hash` is the hash the challenger independently derived from
    /// the same off-chain claim — it must match, proving they checked the same
    /// thing (not a different claim).
    pub fn challenge_claim(
        ctx: Context<ChallengeClaim>,
        recomputed_hash: [u8; 32],
        stake: u64,
    ) -> Result<()> {
        let window = ctx.accounts.config.challenge_window_secs;
        let claim = &mut ctx.accounts.claim;
        require!(claim.status == ClaimStatus::Pending, VeritasError::NotChallengeable);
        let now = Clock::get()?.unix_timestamp;
        require!(
            now <= claim.created_at.saturating_add(window),
            VeritasError::ChallengeWindowClosed
        );
        require!(stake > 0, VeritasError::ZeroStake);
        require!(
            recomputed_hash == claim.inputs_hash,
            VeritasError::HashMismatch
        );

        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.challenger.to_account_info(),
                    to: ctx.accounts.challenge.to_account_info(),
                },
            ),
            stake,
        )?;

        let challenge = &mut ctx.accounts.challenge;
        challenge.claim = claim.key();
        challenge.challenger = ctx.accounts.challenger.key();
        challenge.stake = stake;
        challenge.recomputed_hash = recomputed_hash;
        challenge.resolved = false;
        challenge.bump = ctx.bumps.challenge;
        challenge.created_at = now;

        claim.status = ClaimStatus::Challenged;
        claim.challenger = Some(ctx.accounts.challenger.key());

        emit!(ClaimChallenged {
            claim: claim.key(),
            challenger: challenge.challenger,
            stake,
        });
        Ok(())
    }

    /// Settle a challenged claim with the score the resolver re-derived from
    /// the committed inputs.
    ///
    /// - implausible (score <= threshold) -> bond + stake go to the challenger.
    /// - plausible                        -> bond + stake go to the submitter.
    pub fn resolve(ctx: Context<Resolve>, resolved_score_bps: u16) -> Result<()> {
        require!(resolved_score_bps <= 10_000, VeritasError::BadScore);
        let claim = &mut ctx.accounts.claim;
        let challenge = &mut ctx.accounts.challenge;
        require!(claim.status == ClaimStatus::Challenged, VeritasError::NotResolvable);
        require!(!challenge.resolved, VeritasError::AlreadyResolved);

        let slashed = resolved_score_bps <= IMPLAUSIBLE_THRESHOLD_BPS;
        let (bond, stake) = (claim.bond, challenge.stake);
        let winner = if slashed {
            ctx.accounts.challenger.to_account_info()
        } else {
            ctx.accounts.submitter.to_account_info()
        };
        move_lamports(&claim.to_account_info(), &winner, bond)?;
        move_lamports(&challenge.to_account_info(), &winner, stake)?;

        claim.status = if slashed {
            ClaimStatus::Slashed
        } else {
            ClaimStatus::Confirmed
        };
        claim.bond = 0;
        claim.resolved_score_bps = Some(resolved_score_bps);
        challenge.stake = 0;
        challenge.resolved = true;

        emit!(ClaimResolved {
            claim: claim.key(),
            slashed,
            submitted_score_bps: claim.integrity_score_bps,
            final_score_bps: resolved_score_bps,
        });
        Ok(())
    }

    /// Return the bond of a claim nobody challenged before its window closed.
    pub fn withdraw_bond(ctx: Context<WithdrawBond>) -> Result<()> {
        let window = ctx.accounts.config.challenge_window_secs;
        let claim = &mut ctx.accounts.claim;
        require!(claim.status == ClaimStatus::Pending, VeritasError::NotWithdrawable);
        let now = Clock::get()?.unix_timestamp;
        require!(
            now > claim.created_at.saturating_add(window),
            VeritasError::ChallengeWindowOpen
        );

        let bond = claim.bond;
        move_lamports(
            &claim.to_account_info(),
            &ctx.accounts.submitter.to_account_info(),
            bond,
        )?;
        claim.bond = 0;
        claim.status = ClaimStatus::Finalized;

        emit!(BondWithdrawn {
            claim: claim.key(),
            submitter: claim.submitter,
            amount: bond,
        });
        Ok(())
    }

    /// Refund both sides of a challenge the resolver did not settle before its
    /// deadline. Anyone can crank this.
    pub fn refund_expired_challenge(ctx: Context<RefundExpiredChallenge>) -> Result<()> {
        let window = ctx.accounts.config.resolve_window_secs;
        let claim = &mut ctx.accounts.claim;
        let challenge = &mut ctx.accounts.challenge;
        require!(claim.status == ClaimStatus::Challenged, VeritasError::NotResolvable);
        require!(!challenge.resolved, VeritasError::AlreadyResolved);
        let now = Clock::get()?.unix_timestamp;
        require!(
            now > challenge.created_at.saturating_add(window),
            VeritasError::ResolveWindowOpen
        );

        let (bond, stake) = (claim.bond, challenge.stake);
        move_lamports(
            &claim.to_account_info(),
            &ctx.accounts.submitter.to_account_info(),
            bond,
        )?;
        move_lamports(
            &challenge.to_account_info(),
            &ctx.accounts.challenger.to_account_info(),
            stake,
        )?;

        claim.bond = 0;
        claim.status = ClaimStatus::Unresolved;
        challenge.stake = 0;
        challenge.resolved = true;

        emit!(ChallengeRefunded {
            claim: claim.key(),
            submitter: claim.submitter,
            challenger: challenge.challenger,
            bond,
            stake,
        });
        Ok(())
    }
}

/// Move lamports out of a program-owned account. Each balance is borrowed only
/// for its own update, so `from` and `to` may be the same account.
fn move_lamports(from: &AccountInfo, to: &AccountInfo, amount: u64) -> Result<()> {
    let from_balance = from.lamports();
    **from.try_borrow_mut_lamports()? = from_balance
        .checked_sub(amount)
        .ok_or(VeritasError::InsufficientFunds)?;
    let to_balance = to.lamports();
    **to.try_borrow_mut_lamports()? = to_balance
        .checked_add(amount)
        .ok_or(VeritasError::InsufficientFunds)?;
    Ok(())
}

// --------------------------------------------------------------------------- //
// Accounts
// --------------------------------------------------------------------------- //

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = Config::SPACE,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, Config>,
    #[account(
        constraint = program.programdata_address()? == Some(program_data.key())
            @ VeritasError::Unauthorized
    )]
    pub program: Program<'info, Veritas>,
    #[account(
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ VeritasError::Unauthorized
    )]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetResolver<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        constraint = program.programdata_address()? == Some(program_data.key())
            @ VeritasError::Unauthorized
    )]
    pub program: Program<'info, Veritas>,
    #[account(
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ VeritasError::Unauthorized
    )]
    pub program_data: Account<'info, ProgramData>,
}

#[derive(Accounts)]
#[instruction(inputs_hash: [u8; 32])]
pub struct SubmitClaim<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(
        init,
        payer = submitter,
        space = Claim::SPACE,
        seeds = [b"claim", submitter.key().as_ref(), inputs_hash.as_ref()],
        bump
    )]
    pub claim: Account<'info, Claim>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ChallengeClaim<'info> {
    #[account(mut)]
    pub challenger: Signer<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub claim: Account<'info, Claim>,
    #[account(
        init,
        payer = challenger,
        space = Challenge::SPACE,
        seeds = [b"challenge", claim.key().as_ref()],
        bump
    )]
    pub challenge: Account<'info, Challenge>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Resolve<'info> {
    /// The configured resolver, who re-derived the score from the committed inputs.
    pub resolver: Signer<'info>,
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = resolver @ VeritasError::Unauthorized,
    )]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"claim", claim.submitter.as_ref(), claim.inputs_hash.as_ref()],
        bump = claim.bump,
    )]
    pub claim: Account<'info, Claim>,
    #[account(
        mut,
        seeds = [b"challenge", claim.key().as_ref()],
        bump = challenge.bump,
        constraint = challenge.claim == claim.key() @ VeritasError::ChallengeMismatch,
    )]
    pub challenge: Account<'info, Challenge>,
    /// CHECK: validated to equal claim.submitter; receives funds if the claim holds.
    #[account(mut, address = claim.submitter @ VeritasError::WrongSubmitter)]
    pub submitter: UncheckedAccount<'info>,
    /// CHECK: validated to equal the recorded challenger; receives slashed funds.
    /// `challenge` is bound to `claim` by its seeds, and `challenge.challenger`
    /// is set together with `claim.challenger` in `challenge_claim`.
    #[account(mut, address = challenge.challenger @ VeritasError::WrongChallenger)]
    pub challenger: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct WithdrawBond<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"claim", submitter.key().as_ref(), claim.inputs_hash.as_ref()],
        bump = claim.bump,
        has_one = submitter @ VeritasError::WrongSubmitter,
    )]
    pub claim: Account<'info, Claim>,
}

#[derive(Accounts)]
pub struct RefundExpiredChallenge<'info> {
    /// Anyone can crank a refund once the resolution deadline has passed.
    pub cranker: Signer<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"claim", claim.submitter.as_ref(), claim.inputs_hash.as_ref()],
        bump = claim.bump,
    )]
    pub claim: Account<'info, Claim>,
    #[account(
        mut,
        seeds = [b"challenge", claim.key().as_ref()],
        bump = challenge.bump,
        constraint = challenge.claim == claim.key() @ VeritasError::ChallengeMismatch,
    )]
    pub challenge: Account<'info, Challenge>,
    /// CHECK: validated to equal claim.submitter; receives the refunded bond.
    #[account(mut, address = claim.submitter @ VeritasError::WrongSubmitter)]
    pub submitter: UncheckedAccount<'info>,
    /// CHECK: validated to equal the recorded challenger; receives the refunded stake.
    #[account(mut, address = challenge.challenger @ VeritasError::WrongChallenger)]
    pub challenger: UncheckedAccount<'info>,
}

// --------------------------------------------------------------------------- //
// State
// --------------------------------------------------------------------------- //

#[account]
pub struct Config {
    pub authority: Pubkey,          // 32
    pub resolver: Pubkey,           // 32
    pub challenge_window_secs: i64, // 8
    pub resolve_window_secs: i64,   // 8
    pub bump: u8,                   // 1
}

impl Config {
    // 8 discriminator + fields, padded generously.
    pub const SPACE: usize = 8 + 32 + 32 + 8 + 8 + 1 + 16;
}

#[account]
pub struct Claim {
    pub submitter: Pubkey,               // 32
    pub inputs_hash: [u8; 32],           // 32
    pub model_version: u32,              // 4
    pub claimed_co2_kg: u64,             // 8
    pub integrity_score_bps: u16,        // 2
    pub bond: u64,                       // 8
    pub status: ClaimStatus,             // 1 + 0 (enum, C-like)
    pub challenger: Option<Pubkey>,      // 1 + 32
    pub created_at: i64,                 // 8
    pub bump: u8,                        // 1
    pub resolved_score_bps: Option<u16>, // 1 + 2
}

impl Claim {
    // 8 discriminator + fields, padded generously.
    pub const SPACE: usize =
        8 + 32 + 32 + 4 + 8 + 2 + 8 + 1 + (1 + 32) + 8 + 1 + (1 + 2) + 16;
}

#[account]
pub struct Challenge {
    pub claim: Pubkey,             // 32
    pub challenger: Pubkey,        // 32
    pub stake: u64,                // 8
    pub recomputed_hash: [u8; 32], // 32
    pub resolved: bool,            // 1
    pub bump: u8,                  // 1
    pub created_at: i64,           // 8
}

impl Challenge {
    pub const SPACE: usize = 8 + 32 + 32 + 8 + 32 + 1 + 1 + 8 + 8;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Pending,
    Challenged,
    Confirmed,
    Slashed,
    /// Unchallenged when its window closed; the bond has been withdrawn.
    Finalized,
    /// The resolver missed its deadline; both sides were refunded.
    Unresolved,
}

// --------------------------------------------------------------------------- //
// Events
// --------------------------------------------------------------------------- //

#[event]
pub struct ClaimSubmitted {
    pub claim: Pubkey,
    pub submitter: Pubkey,
    pub inputs_hash: [u8; 32],
    pub integrity_score_bps: u16,
}

#[event]
pub struct ClaimChallenged {
    pub claim: Pubkey,
    pub challenger: Pubkey,
    pub stake: u64,
}

#[event]
pub struct ClaimResolved {
    pub claim: Pubkey,
    pub slashed: bool,
    pub submitted_score_bps: u16,
    pub final_score_bps: u16,
}

#[event]
pub struct BondWithdrawn {
    pub claim: Pubkey,
    pub submitter: Pubkey,
    pub amount: u64,
}

#[event]
pub struct ChallengeRefunded {
    pub claim: Pubkey,
    pub submitter: Pubkey,
    pub challenger: Pubkey,
    pub bond: u64,
    pub stake: u64,
}

#[event]
pub struct ResolverChanged {
    pub config: Pubkey,
    pub previous: Pubkey,
    pub resolver: Pubkey,
}

// --------------------------------------------------------------------------- //
// Errors
// --------------------------------------------------------------------------- //

#[error_code]
pub enum VeritasError {
    #[msg("Bond is below the minimum required stake")]
    BondTooLow,
    #[msg("Integrity score must be 0..10000 basis points")]
    BadScore,
    #[msg("Claim is not in a challengeable state")]
    NotChallengeable,
    #[msg("Stake must be greater than zero")]
    ZeroStake,
    #[msg("Recomputed hash does not match the committed inputs hash")]
    HashMismatch,
    #[msg("Claim is not in a resolvable state")]
    NotResolvable,
    #[msg("Challenge already resolved")]
    AlreadyResolved,
    #[msg("Challenge does not belong to this claim")]
    ChallengeMismatch,
    #[msg("Submitter account does not match the claim")]
    WrongSubmitter,
    #[msg("Challenger account does not match the claim")]
    WrongChallenger,
    #[msg("Signer is not authorized for this action")]
    Unauthorized,
    #[msg("Windows must be longer than zero seconds")]
    BadWindow,
    #[msg("The challenge window for this claim has closed")]
    ChallengeWindowClosed,
    #[msg("The challenge window for this claim is still open")]
    ChallengeWindowOpen,
    #[msg("The resolver's deadline for this challenge has not passed")]
    ResolveWindowOpen,
    #[msg("Only an unchallenged claim's bond can be withdrawn")]
    NotWithdrawable,
    #[msg("Account balance is too low for this transfer")]
    InsufficientFunds,
}
