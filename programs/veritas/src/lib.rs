//! Veritas — on-chain plausibility commitment & challenge protocol.
//!
//! An *optimistic* verification game. The owner of a registered asset commits a
//! physics-scored claim (only its hash + headline figures go on-chain; the full
//! claim stays off-chain and cheap) and escrows a bond. Anyone can challenge it
//! by staking while the challenge window is open. A challenged claim is settled
//! by a committee of independent re-runners: each member re-runs the
//! deterministic physics engine on the committed inputs and submits the score it
//! derives, and the claim settles only when a quorum reports the same score.
//!
//! MVP honesty: a quorum of the configured committee can still be wrong or
//! collude, and the committee is appointed by the program's upgrade authority.
//! What it cannot do is take the money — funds only ever move to the submitter
//! or the challenger. Replacing the committee with verifiable compute, so the
//! chain checks the computation itself, is the post-hackathon research problem.
//!
//! Funds never stay locked: an unchallenged bond can be withdrawn once the
//! challenge window closes, a settled claim pays out immediately, a committee
//! that splits refunds both sides, and anyone can refund a challenge the
//! committee leaves past its deadline.
//!
//! Each claim is pinned to one registered asset and one settlement slot, so the
//! same half-hour of generation cannot be claimed twice, and the capacity a
//! claim is judged against is the registry's figure rather than the submitter's.
//!
//! Built against Anchor 0.30.x.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("DypSeezrbcEhDSJNganfpjjkQXp1NBAHpvDAQrQBHLEW");

use crate::program::Veritas;

/// Committee scores at or below this (basis points, 0..10000) are implausible.
/// 0.50 -> 5000 bps.
pub const IMPLAUSIBLE_THRESHOLD_BPS: u16 = 5000;

/// Minimum bond a submitter must stake (lamports). 0.05 SOL.
pub const MIN_BOND_LAMPORTS: u64 = 50_000_000;

/// Upper bound on committee size, so Config stays a fixed-size account.
pub const MAX_RESOLVERS: usize = 5;

/// Half-hourly settlement slots, in seconds.
pub const SETTLEMENT_SLOT_SECS: i64 = 1800;

#[program]
pub mod veritas {
    use super::*;

    /// One-time setup, callable only by the program's upgrade authority: names
    /// the resolver committee, the quorum that settles a dispute, and the
    /// challenge and resolution windows.
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        resolvers: Vec<Pubkey>,
        quorum: u8,
        challenge_window_secs: i64,
        resolve_window_secs: i64,
    ) -> Result<()> {
        require_gt!(challenge_window_secs, 0, VeritasError::BadWindow);
        require_gt!(resolve_window_secs, 0, VeritasError::BadWindow);
        validate_committee(&resolvers, quorum)?;

        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.resolvers = resolvers;
        config.quorum = quorum;
        config.challenge_window_secs = challenge_window_secs;
        config.resolve_window_secs = resolve_window_secs;
        config.bump = ctx.bumps.config;
        Ok(())
    }

    /// Replace the committee and quorum. Callable only by the program's upgrade
    /// authority. In-flight challenges keep the quorum in force at their next
    /// resolution, so shrinking a committee cannot strand them.
    pub fn set_resolvers(
        ctx: Context<SetResolvers>,
        resolvers: Vec<Pubkey>,
        quorum: u8,
    ) -> Result<()> {
        validate_committee(&resolvers, quorum)?;

        let config = &mut ctx.accounts.config;
        config.resolvers = resolvers.clone();
        config.quorum = quorum;

        emit!(CommitteeChanged {
            config: config.key(),
            resolvers,
            quorum,
        });
        Ok(())
    }

    /// Register an asset the engine can be pointed at. Callable only by the
    /// program's upgrade authority: the registry is what stops a submitter
    /// inventing a 1 GW solar farm, or claiming another operator's output.
    pub fn register_asset(
        ctx: Context<RegisterAsset>,
        asset_id: [u8; 32],
        owner: Pubkey,
        asset_type: u8,
        nameplate_capacity_kw: u64,
        region_id: u16,
        latitude_millideg: i32,
    ) -> Result<()> {
        require_gt!(nameplate_capacity_kw, 0, VeritasError::BadAsset);
        require!(
            (1..=17).contains(&region_id),
            VeritasError::BadAsset
        );
        require!(
            (-90_000..=90_000).contains(&latitude_millideg),
            VeritasError::BadAsset
        );

        let asset = &mut ctx.accounts.asset;
        asset.asset_id = asset_id;
        asset.owner = owner;
        asset.asset_type = asset_type;
        asset.nameplate_capacity_kw = nameplate_capacity_kw;
        asset.region_id = region_id;
        asset.latitude_millideg = latitude_millideg;
        asset.bump = ctx.bumps.asset;

        emit!(AssetRegistered {
            asset: asset.key(),
            asset_id,
            owner,
            nameplate_capacity_kw,
        });
        Ok(())
    }

    /// Commit a physics-scored claim for one settlement slot of one registered
    /// asset. Escrows `bond` in the claim PDA.
    ///
    /// The `Reading` PDA is seeded by the asset and the slot, so a second claim
    /// for the same half-hour of the same asset cannot be created at all.
    ///
    /// `integrity_score_bps` is the submitter's own engine score. It is recorded
    /// for reference; a dispute is settled by the committee's score instead.
    pub fn submit_claim(
        ctx: Context<SubmitClaim>,
        asset_id: [u8; 32],
        inputs_hash: [u8; 32],
        model_version: u32,
        period_start: i64,
        claimed_co2_kg: u64,
        integrity_score_bps: u16,
        bond: u64,
    ) -> Result<()> {
        require!(bond >= MIN_BOND_LAMPORTS, VeritasError::BondTooLow);
        require!(integrity_score_bps <= 10_000, VeritasError::BadScore);
        require!(
            period_start > 0 && period_start % SETTLEMENT_SLOT_SECS == 0,
            VeritasError::BadPeriod
        );
        let now = Clock::get()?.unix_timestamp;
        require!(
            period_start.saturating_add(SETTLEMENT_SLOT_SECS) <= now,
            VeritasError::PeriodNotFinished
        );

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

        let asset_key = ctx.accounts.asset.key();
        let claim = &mut ctx.accounts.claim;
        claim.submitter = ctx.accounts.submitter.key();
        claim.asset = asset_key;
        claim.inputs_hash = inputs_hash;
        claim.model_version = model_version;
        claim.period_start = period_start;
        claim.claimed_co2_kg = claimed_co2_kg;
        claim.integrity_score_bps = integrity_score_bps;
        claim.bond = bond;
        claim.status = ClaimStatus::Pending;
        claim.challenger = None;
        claim.created_at = now;
        claim.bump = ctx.bumps.claim;
        claim.resolved_score_bps = None;

        let reading = &mut ctx.accounts.reading;
        reading.asset = asset_key;
        reading.claim = claim.key();
        reading.period_start = period_start;
        reading.bump = ctx.bumps.reading;

        emit!(ClaimSubmitted {
            claim: claim.key(),
            submitter: claim.submitter,
            asset: asset_key,
            asset_id,
            period_start,
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
        challenge.votes = Vec::new();

        claim.status = ClaimStatus::Challenged;
        claim.challenger = Some(ctx.accounts.challenger.key());

        emit!(ClaimChallenged {
            claim: claim.key(),
            challenger: challenge.challenger,
            stake,
        });
        Ok(())
    }

    /// Submit one committee member's re-derived score for a challenged claim.
    ///
    /// The claim settles as soon as `quorum` members report the *same* score:
    /// implausible (score <= threshold) pays the challenger, plausible pays the
    /// submitter. If every member has voted and no quorum agrees, both sides are
    /// refunded rather than left waiting on an agreement that cannot come.
    pub fn submit_resolution(ctx: Context<SubmitResolution>, score_bps: u16) -> Result<()> {
        require!(score_bps <= 10_000, VeritasError::BadScore);
        let (quorum, committee_size) = {
            let config = &ctx.accounts.config;
            let resolver = ctx.accounts.resolver.key();
            require!(config.resolvers.contains(&resolver), VeritasError::Unauthorized);
            (config.quorum, config.resolvers.len() as u8)
        };

        let resolver = ctx.accounts.resolver.key();
        let claim = &mut ctx.accounts.claim;
        let challenge = &mut ctx.accounts.challenge;
        require!(claim.status == ClaimStatus::Challenged, VeritasError::NotResolvable);
        require!(!challenge.resolved, VeritasError::AlreadyResolved);
        require!(
            challenge.votes.iter().all(|vote| vote.resolver != resolver),
            VeritasError::AlreadyVoted
        );

        challenge.votes.push(ResolutionVote { resolver, score_bps });
        let agreeing = challenge
            .votes
            .iter()
            .filter(|vote| vote.score_bps == score_bps)
            .count() as u8;
        let votes_cast = challenge.votes.len() as u8;

        emit!(ResolutionSubmitted {
            claim: claim.key(),
            resolver,
            score_bps,
            votes_cast,
            agreeing,
            quorum,
        });

        if agreeing >= quorum {
            let slashed = score_bps <= IMPLAUSIBLE_THRESHOLD_BPS;
            let winner = if slashed {
                ctx.accounts.challenger.to_account_info()
            } else {
                ctx.accounts.submitter.to_account_info()
            };
            let (bond, stake) = (claim.bond, challenge.stake);
            move_lamports(&claim.to_account_info(), &winner, bond)?;
            move_lamports(&challenge.to_account_info(), &winner, stake)?;

            claim.status = if slashed {
                ClaimStatus::Slashed
            } else {
                ClaimStatus::Confirmed
            };
            claim.bond = 0;
            claim.resolved_score_bps = Some(score_bps);
            challenge.stake = 0;
            challenge.resolved = true;

            emit!(ClaimResolved {
                claim: claim.key(),
                slashed,
                submitted_score_bps: claim.integrity_score_bps,
                final_score_bps: score_bps,
                agreeing,
            });
        } else if votes_cast >= committee_size {
            // The committee saw the same inputs and disagreed. Nobody wins a
            // dispute on a split, and nothing is held: refund both sides.
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
                reason: RefundReason::CommitteeSplit,
            });
        }
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

    /// Refund both sides of a challenge the committee did not settle before its
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
            reason: RefundReason::CommitteeSilent,
        });
        Ok(())
    }
}

/// A committee must be non-empty, free of duplicates, no larger than the account
/// can hold, and its quorum must be reachable.
fn validate_committee(resolvers: &[Pubkey], quorum: u8) -> Result<()> {
    require!(!resolvers.is_empty(), VeritasError::BadCommittee);
    require!(resolvers.len() <= MAX_RESOLVERS, VeritasError::BadCommittee);
    require!(
        quorum >= 1 && quorum as usize <= resolvers.len(),
        VeritasError::BadQuorum
    );
    for (i, resolver) in resolvers.iter().enumerate() {
        require!(
            !resolvers[i + 1..].contains(resolver),
            VeritasError::BadCommittee
        );
    }
    Ok(())
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
        seeds = [b"config-v2"],
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
pub struct SetResolvers<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [b"config-v2"], bump = config.bump)]
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
#[instruction(asset_id: [u8; 32])]
pub struct RegisterAsset<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = Asset::SPACE,
        seeds = [b"asset", asset_id.as_ref()],
        bump
    )]
    pub asset: Account<'info, Asset>,
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
#[instruction(asset_id: [u8; 32], inputs_hash: [u8; 32], model_version: u32, period_start: i64)]
pub struct SubmitClaim<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(
        seeds = [b"asset", asset_id.as_ref()],
        bump = asset.bump,
        constraint = asset.owner == submitter.key() @ VeritasError::NotAssetOwner,
    )]
    pub asset: Account<'info, Asset>,
    #[account(
        init,
        payer = submitter,
        space = Reading::SPACE,
        seeds = [b"reading", asset_id.as_ref(), &period_start.to_le_bytes()],
        bump
    )]
    pub reading: Account<'info, Reading>,
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
    #[account(seeds = [b"config-v2"], bump = config.bump)]
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
pub struct SubmitResolution<'info> {
    /// A committee member, re-running the engine on the committed inputs.
    pub resolver: Signer<'info>,
    #[account(seeds = [b"config-v2"], bump = config.bump)]
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
    #[account(seeds = [b"config-v2"], bump = config.bump)]
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
    #[account(seeds = [b"config-v2"], bump = config.bump)]
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
    pub resolvers: Vec<Pubkey>,     // 4 + 32 * MAX_RESOLVERS
    pub quorum: u8,                 // 1
    pub challenge_window_secs: i64, // 8
    pub resolve_window_secs: i64,   // 8
    pub bump: u8,                   // 1
}

impl Config {
    // 8 discriminator + fields, padded generously.
    pub const SPACE: usize = 8 + 32 + (4 + 32 * MAX_RESOLVERS) + 1 + 8 + 8 + 1 + 16;
}

#[account]
pub struct Asset {
    pub asset_id: [u8; 32],          // 32
    pub owner: Pubkey,               // 32
    pub asset_type: u8,              // 1  (0 solar_pv, 1 wind, 2 battery_export)
    pub nameplate_capacity_kw: u64,  // 8
    pub region_id: u16,              // 2  (NESO DNO region, 1..17)
    pub latitude_millideg: i32,      // 4
    pub bump: u8,                    // 1
}

impl Asset {
    pub const SPACE: usize = 8 + 32 + 32 + 1 + 8 + 2 + 4 + 1 + 16;
}

/// One settlement slot of one asset, claimed once. Its PDA existing is the
/// double-counting check: a second claim for the same slot cannot init it.
#[account]
pub struct Reading {
    pub asset: Pubkey,     // 32
    pub claim: Pubkey,     // 32
    pub period_start: i64, // 8
    pub bump: u8,          // 1
}

impl Reading {
    pub const SPACE: usize = 8 + 32 + 32 + 8 + 1 + 16;
}

#[account]
pub struct Claim {
    pub submitter: Pubkey,               // 32
    pub asset: Pubkey,                   // 32
    pub inputs_hash: [u8; 32],           // 32
    pub model_version: u32,              // 4
    pub period_start: i64,               // 8
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
        8 + 32 + 32 + 32 + 4 + 8 + 8 + 2 + 8 + 1 + (1 + 32) + 8 + 1 + (1 + 2) + 16;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionVote {
    pub resolver: Pubkey, // 32
    pub score_bps: u16,   // 2
}

#[account]
pub struct Challenge {
    pub claim: Pubkey,               // 32
    pub challenger: Pubkey,          // 32
    pub stake: u64,                  // 8
    pub recomputed_hash: [u8; 32],   // 32
    pub resolved: bool,              // 1
    pub bump: u8,                    // 1
    pub created_at: i64,             // 8
    pub votes: Vec<ResolutionVote>,  // 4 + 34 * MAX_RESOLVERS
}

impl Challenge {
    pub const SPACE: usize =
        8 + 32 + 32 + 8 + 32 + 1 + 1 + 8 + (4 + 34 * MAX_RESOLVERS) + 8;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Pending,
    Challenged,
    Confirmed,
    Slashed,
    /// Unchallenged when its window closed; the bond has been withdrawn.
    Finalized,
    /// The committee split or went quiet; both sides were refunded.
    Unresolved,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum RefundReason {
    /// Every member voted and no quorum agreed on one score.
    CommitteeSplit,
    /// The committee did not settle before the resolution deadline.
    CommitteeSilent,
}

// --------------------------------------------------------------------------- //
// Events
// --------------------------------------------------------------------------- //

#[event]
pub struct AssetRegistered {
    pub asset: Pubkey,
    pub asset_id: [u8; 32],
    pub owner: Pubkey,
    pub nameplate_capacity_kw: u64,
}

#[event]
pub struct ClaimSubmitted {
    pub claim: Pubkey,
    pub submitter: Pubkey,
    pub asset: Pubkey,
    pub asset_id: [u8; 32],
    pub period_start: i64,
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
pub struct ResolutionSubmitted {
    pub claim: Pubkey,
    pub resolver: Pubkey,
    pub score_bps: u16,
    pub votes_cast: u8,
    pub agreeing: u8,
    pub quorum: u8,
}

#[event]
pub struct ClaimResolved {
    pub claim: Pubkey,
    pub slashed: bool,
    pub submitted_score_bps: u16,
    pub final_score_bps: u16,
    pub agreeing: u8,
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
    pub reason: RefundReason,
}

#[event]
pub struct CommitteeChanged {
    pub config: Pubkey,
    pub resolvers: Vec<Pubkey>,
    pub quorum: u8,
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
    #[msg("The committee's deadline for this challenge has not passed")]
    ResolveWindowOpen,
    #[msg("Only an unchallenged claim's bond can be withdrawn")]
    NotWithdrawable,
    #[msg("Account balance is too low for this transfer")]
    InsufficientFunds,
    #[msg("This resolver has already voted on this challenge")]
    AlreadyVoted,
    #[msg("Committee must be 1..5 distinct resolvers")]
    BadCommittee,
    #[msg("Quorum must be between 1 and the committee size")]
    BadQuorum,
    #[msg("Asset capacity, region or latitude is out of range")]
    BadAsset,
    #[msg("Only the registered owner of an asset can claim its output")]
    NotAssetOwner,
    #[msg("Period must start on a half-hourly settlement boundary")]
    BadPeriod,
    #[msg("The settlement period has not finished yet")]
    PeriodNotFinished,
}
