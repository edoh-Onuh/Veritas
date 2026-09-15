//! Veritas — on-chain plausibility commitment & challenge protocol.
//!
//! An *optimistic* verification game. A submitter commits a physics-scored
//! claim (only its hash + headline figures go on-chain; the full claim stays
//! off-chain and cheap). Anyone can challenge by staking; resolution is
//! deterministic because the off-chain physics engine is deterministic — the
//! same committed inputs always produce the same verdict.
//!
//! MVP honesty: `resolve` trusts that the integrity score committed at submit
//! time was correctly derived from the committed inputs. Making the computation
//! itself trustlessly verifiable on-chain (verifiable compute / a committee of
//! independent re-runners) is the core post-hackathon research problem.
//!
//! Built against Anchor 0.30.x. Replace the program id below with the one
//! `anchor keys list` prints after your first build.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("DypSeezrbcEhDSJNganfpjjkQXp1NBAHpvDAQrQBHLEW");

/// Scores at or below this (basis points, 0..10000) are considered implausible.
/// 0.50 -> 5000 bps. A submitter committing a score below this is asserting
/// their own claim is dubious, so a challenge against it will succeed.
pub const IMPLAUSIBLE_THRESHOLD_BPS: u16 = 5000;

/// Minimum bond a submitter must stake (lamports). 0.05 SOL.
pub const MIN_BOND_LAMPORTS: u64 = 50_000_000;

#[program]
pub mod veritas {
    use super::*;

    /// Commit a physics-scored claim. Escrows `bond` in the claim PDA.
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

        emit!(ClaimSubmitted {
            claim: claim.key(),
            submitter: claim.submitter,
            inputs_hash,
            integrity_score_bps,
        });
        Ok(())
    }

    /// Dispute a pending claim. Escrows `stake` in the challenge PDA.
    /// `recomputed_hash` is the hash the challenger independently derived from
    /// the same off-chain claim — it must match, proving they checked the same
    /// thing (not a different claim).
    pub fn challenge_claim(
        ctx: Context<ChallengeClaim>,
        recomputed_hash: [u8; 32],
        stake: u64,
    ) -> Result<()> {
        let claim = &mut ctx.accounts.claim;
        require!(claim.status == ClaimStatus::Pending, VeritasError::NotChallengeable);
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

        claim.status = ClaimStatus::Challenged;
        claim.challenger = Some(ctx.accounts.challenger.key());

        emit!(ClaimChallenged {
            claim: claim.key(),
            challenger: challenge.challenger,
            stake,
        });
        Ok(())
    }

    /// Deterministically resolve a challenged claim.
    ///
    /// The chain checks the one thing it can verify cheaply: was the committed
    /// integrity score below the implausibility threshold? Because the physics
    /// engine is deterministic, this is reproducible by anyone.
    ///
    /// - implausible claim  -> submitter slashed; bond + stake go to challenger.
    /// - claim holds up      -> challenger's stake goes to submitter; bond back.
    pub fn resolve(ctx: Context<Resolve>) -> Result<()> {
        let claim = &mut ctx.accounts.claim;
        let challenge = &mut ctx.accounts.challenge;
        require!(claim.status == ClaimStatus::Challenged, VeritasError::NotResolvable);
        require!(!challenge.resolved, VeritasError::AlreadyResolved);
        require!(
            challenge.claim == claim.key(),
            VeritasError::ChallengeMismatch
        );

        let claim_implausible =
            claim.integrity_score_bps <= IMPLAUSIBLE_THRESHOLD_BPS;

        // Lamports held in each PDA (bond in claim, stake in challenge).
        let bond = claim.bond;
        let stake = challenge.stake;

        if claim_implausible {
            // Submitter was lying: challenger takes bond + their stake back.
            **claim.to_account_info().try_borrow_mut_lamports()? -= bond;
            **ctx.accounts.challenger.to_account_info()
                .try_borrow_mut_lamports()? += bond;
            **challenge.to_account_info().try_borrow_mut_lamports()? -= stake;
            **ctx.accounts.challenger.to_account_info()
                .try_borrow_mut_lamports()? += stake;
            claim.status = ClaimStatus::Slashed;
        } else {
            // Claim holds: submitter keeps bond, takes challenger's stake.
            **challenge.to_account_info().try_borrow_mut_lamports()? -= stake;
            **ctx.accounts.submitter.to_account_info()
                .try_borrow_mut_lamports()? += stake;
            // bond stays in the claim PDA and is reclaimable by submitter later
            claim.status = ClaimStatus::Confirmed;
        }

        claim.bond = 0;
        challenge.stake = 0;
        challenge.resolved = true;

        emit!(ClaimResolved {
            claim: claim.key(),
            slashed: claim_implausible,
            final_score_bps: claim.integrity_score_bps,
        });
        Ok(())
    }
}

// --------------------------------------------------------------------------- //
// Accounts
// --------------------------------------------------------------------------- //

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
    /// Anyone can crank resolution; it's deterministic.
    pub cranker: Signer<'info>,
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
    /// CHECK: validated to equal claim.submitter; receives refunded stake/bond.
    #[account(mut, address = claim.submitter @ VeritasError::WrongSubmitter)]
    pub submitter: UncheckedAccount<'info>,
    /// CHECK: validated to equal the recorded challenger; receives slashed funds.
    /// `challenge` is bound to `claim` by its seeds, and `challenge.challenger`
    /// is set together with `claim.challenger` in `challenge_claim`.
    #[account(mut, address = challenge.challenger @ VeritasError::WrongChallenger)]
    pub challenger: UncheckedAccount<'info>,
}

// --------------------------------------------------------------------------- //
// State
// --------------------------------------------------------------------------- //

#[account]
pub struct Claim {
    pub submitter: Pubkey,          // 32
    pub inputs_hash: [u8; 32],      // 32
    pub model_version: u32,         // 4
    pub claimed_co2_kg: u64,        // 8
    pub integrity_score_bps: u16,   // 2
    pub bond: u64,                  // 8
    pub status: ClaimStatus,        // 1 + 0 (enum, C-like)
    pub challenger: Option<Pubkey>, // 1 + 32
    pub created_at: i64,            // 8
    pub bump: u8,                   // 1
}

impl Claim {
    // 8 discriminator + fields, padded generously.
    pub const SPACE: usize = 8 + 32 + 32 + 4 + 8 + 2 + 8 + 1 + (1 + 32) + 8 + 1 + 16;
}

#[account]
pub struct Challenge {
    pub claim: Pubkey,              // 32
    pub challenger: Pubkey,         // 32
    pub stake: u64,                // 8
    pub recomputed_hash: [u8; 32], // 32
    pub resolved: bool,            // 1
    pub bump: u8,                  // 1
}

impl Challenge {
    pub const SPACE: usize = 8 + 32 + 32 + 8 + 32 + 1 + 1 + 8;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Pending,
    Challenged,
    Confirmed,
    Slashed,
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
    pub final_score_bps: u16,
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
}
