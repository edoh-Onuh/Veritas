/**
 * Anchor integration tests for the Veritas program.
 *
 * Run with:  anchor test
 * (spins up a local validator, deploys, executes these against it)
 *
 * These assert the properties the security review asked for: a dispute is
 * settled by a quorum of the committee rather than by the submitter's own
 * score, a claim is pinned to a registered asset and a single settlement slot,
 * and no bond or stake can be left stranded — not by silence, not by a split.
 */

import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Veritas } from "../../target/types/veritas";
import { assert } from "chai";
import { createHash } from "crypto";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";

// Short windows so the timeout paths run in seconds on a local validator.
const CHALLENGE_WINDOW_SECS = 10;
const RESOLVE_WINDOW_SECS = 10;
const QUORUM = 2;
const SLOT_SECS = 1800;
const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

describe("veritas", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.Veritas as Program<Veritas>;
  const connection = provider.connection;

  // Three independent re-runners; any two that agree settle a dispute.
  const committee = [Keypair.generate(), Keypair.generate(), Keypair.generate()];
  const [config] = PublicKey.findProgramAddressSync(
    [Buffer.from("config-v2")],
    program.programId
  );
  const [programData] = PublicKey.findProgramAddressSync(
    [program.programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE
  );
  const bond = 0.1 * LAMPORTS_PER_SOL;
  const stake = 0.05 * LAMPORTS_PER_SOL;
  const confirmed = { commitment: "confirmed" as const };

  const sha256 = (seed: string): number[] =>
    Array.from(createHash("sha256").update(seed).digest());

  async function fund(kp: Keypair, sol = 2) {
    const sig = await connection.requestAirdrop(
      kp.publicKey,
      sol * LAMPORTS_PER_SOL
    );
    await connection.confirmTransaction(sig, "confirmed");
  }

  const balance = (key: PublicKey) => connection.getBalance(key, "confirmed");
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  /** A settlement slot that has already finished. */
  const finishedSlot = (slotsAgo = 2) =>
    Math.floor(Date.now() / 1000 / SLOT_SECS) * SLOT_SECS - slotsAgo * SLOT_SECS;

  const assetPda = (assetId: number[]) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("asset"), Buffer.from(assetId)],
      program.programId
    )[0];

  const readingPda = (assetId: number[], periodStart: number) =>
    PublicKey.findProgramAddressSync(
      [
        Buffer.from("reading"),
        Buffer.from(assetId),
        new anchor.BN(periodStart).toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    )[0];

  const claimPda = (submitter: PublicKey, hash: number[]) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("claim"), submitter.toBuffer(), Buffer.from(hash)],
      program.programId
    )[0];

  const challengePda = (claim: PublicKey) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("challenge"), claim.toBuffer()],
      program.programId
    )[0];

  function errorCode(err: any): string | undefined {
    return (
      err?.error?.errorCode?.code ??
      anchor.AnchorError.parse(err?.logs ?? [])?.error.errorCode.code
    );
  }

  /**
   * Assert an instruction fails with a specific Anchor error code.
   *
   * Anchor 0.30.1 rethrows some send failures through web3.js's newer
   * SendTransactionError, whose object-argument constructor drops the message
   * and the logs ("Unknown action 'undefined'"). Whether that happens depends on
   * where the failure surfaces, so when no code survives the rpc path we replay
   * the same builder through simulate() and read the code off fresh logs.
   */
  async function expectError(builder: any, code: string) {
    let err: any;
    try {
      await builder.rpc(confirmed);
    } catch (e) {
      err = e;
    }
    assert.exists(err, `expected ${code}, but the transaction succeeded`);

    let got = errorCode(err);
    if (got === undefined) {
      try {
        await builder.simulate(confirmed);
      } catch (e) {
        got = errorCode(e);
      }
    }
    assert.equal(got, code, String(err));
  }

  /** Register an asset owned by `owner`; only the upgrade authority may. */
  async function registerAsset(seed: string, owner: PublicKey) {
    const assetId = sha256(seed);
    const asset = assetPda(assetId);
    await program.methods
      .registerAsset(assetId, owner, 0, new anchor.BN(5000), 3, 53_500)
      .accountsPartial({
        authority: provider.wallet.publicKey,
        asset,
        program: program.programId,
        programData,
        systemProgram: SystemProgram.programId,
      })
      .rpc(confirmed);
    return { assetId, asset };
  }

  function submitClaimIx(
    submitter: Keypair,
    assetId: number[],
    asset: PublicKey,
    hash: number[],
    periodStart: number,
    scoreBps: number
  ) {
    return program.methods
      .submitClaim(
        assetId,
        hash,
        1,
        new anchor.BN(periodStart),
        new anchor.BN(1800),
        scoreBps,
        new anchor.BN(bond)
      )
      .accountsPartial({
        submitter: submitter.publicKey,
        asset,
        reading: readingPda(assetId, periodStart),
        claim: claimPda(submitter.publicKey, hash),
        systemProgram: SystemProgram.programId,
      })
      .signers([submitter]);
  }

  function challengeIx(challenger: Keypair, claim: PublicKey, hash: number[]) {
    return program.methods
      .challengeClaim(hash, new anchor.BN(stake))
      .accountsPartial({
        challenger: challenger.publicKey,
        config,
        claim,
        challenge: challengePda(claim),
        systemProgram: SystemProgram.programId,
      })
      .signers([challenger]);
  }

  function voteIx(
    resolver: Keypair,
    scoreBps: number,
    claim: PublicKey,
    submitter: PublicKey,
    challenger: PublicKey
  ) {
    return program.methods
      .submitResolution(scoreBps)
      .accountsPartial({
        resolver: resolver.publicKey,
        config,
        claim,
        challenge: challengePda(claim),
        submitter,
        challenger,
      })
      .signers([resolver]);
  }

  function withdrawIx(submitter: Keypair, claim: PublicKey) {
    return program.methods
      .withdrawBond()
      .accountsPartial({ submitter: submitter.publicKey, config, claim })
      .signers([submitter]);
  }

  function refundIx(
    claim: PublicKey,
    submitter: PublicKey,
    challenger: PublicKey
  ) {
    return program.methods.refundExpiredChallenge().accountsPartial({
      cranker: provider.wallet.publicKey,
      config,
      claim,
      challenge: challengePda(claim),
      submitter,
      challenger,
    });
  }

  /** Register an asset, submit a claim for it, and challenge it. */
  async function disputedClaim(seed: string, submittedScoreBps = 9500) {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);
    const { assetId, asset } = await registerAsset(seed, submitter.publicKey);
    const hash = sha256(`${seed}-claim`);
    const periodStart = finishedSlot();
    await submitClaimIx(
      submitter,
      assetId,
      asset,
      hash,
      periodStart,
      submittedScoreBps
    ).rpc(confirmed);
    const claim = claimPda(submitter.publicKey, hash);
    await challengeIx(challenger, claim, hash).rpc(confirmed);
    return { submitter, challenger, claim, hash, assetId, asset, periodStart };
  }

  const claimStatus = async (claim: PublicKey) =>
    Object.keys((await program.account.claim.fetch(claim)).status)[0];

  before(async () => {
    for (const member of committee) await fund(member);
    // The provider wallet deployed the program, so it is the upgrade authority.
    await program.methods
      .initializeConfig(
        committee.map((m) => m.publicKey),
        QUORUM,
        new anchor.BN(CHALLENGE_WINDOW_SECS),
        new anchor.BN(RESOLVE_WINDOW_SECS)
      )
      .accountsPartial({
        authority: provider.wallet.publicKey,
        config,
        program: program.programId,
        programData,
        systemProgram: SystemProgram.programId,
      })
      .rpc(confirmed);
  });

  it("slashes only once a quorum agrees, whatever score the submitter committed", async () => {
    const { submitter, challenger, claim } = await disputedClaim("slash-1", 9500);

    // One member is not a quorum: the claim stays challenged and nothing moves.
    await voteIx(
      committee[0],
      1500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);
    assert.equal(await claimStatus(claim), "challenged");

    const before = await balance(challenger.publicKey);
    await voteIx(
      committee[1],
      1500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "slashed");
    assert.equal(claimAcc.resolvedScoreBps, 1500);
    assert.equal(claimAcc.integrityScoreBps, 9500); // self-reported, and ignored
    assert.equal((await balance(challenger.publicKey)) - before, bond + stake);
  });

  it("confirms a claim the quorum scores as plausible and pays bond plus stake", async () => {
    const { submitter, challenger, claim } = await disputedClaim("confirm-1", 1500);

    await voteIx(
      committee[0],
      9500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);
    const before = await balance(submitter.publicKey);
    await voteIx(
      committee[2],
      9500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "confirmed");
    assert.equal(claimAcc.resolvedScoreBps, 9500);
    assert.equal(claimAcc.bond.toNumber(), 0);
    assert.equal((await balance(submitter.publicKey)) - before, bond + stake);
  });

  it("rejects a vote from outside the committee", async () => {
    const { submitter, challenger, claim } = await disputedClaim("impostor-1");
    const impostor = Keypair.generate();
    await fund(impostor);

    await expectError(
      voteIx(impostor, 9500, claim, submitter.publicKey, challenger.publicKey),
      "Unauthorized"
    );
    assert.equal(await claimStatus(claim), "challenged");
  });

  it("rejects a second vote from the same committee member", async () => {
    const { submitter, challenger, claim } = await disputedClaim("double-vote-1");

    await voteIx(
      committee[0],
      1500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);
    await expectError(
      voteIx(committee[0], 1500, claim, submitter.publicKey, challenger.publicKey),
      "AlreadyVoted"
    );
    assert.equal(await claimStatus(claim), "challenged");
  });

  it("refunds both sides when the committee splits", async () => {
    const { submitter, challenger, claim } = await disputedClaim("split-1");

    await voteIx(
      committee[0],
      1500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);
    await voteIx(
      committee[1],
      9500,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const submitterBefore = await balance(submitter.publicKey);
    const challengerBefore = await balance(challenger.publicKey);
    await voteIx(
      committee[2],
      4000,
      claim,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    assert.equal(await claimStatus(claim), "unresolved");
    assert.equal((await balance(submitter.publicKey)) - submitterBefore, bond);
    assert.equal((await balance(challenger.publicKey)) - challengerBefore, stake);
  });

  it("only lets the registered owner claim an asset's output", async () => {
    const owner = Keypair.generate();
    const interloper = Keypair.generate();
    await fund(owner);
    await fund(interloper);
    const { assetId, asset } = await registerAsset("owner-binding-1", owner.publicKey);

    await expectError(
      submitClaimIx(
        interloper,
        assetId,
        asset,
        sha256("owner-binding-1-claim"),
        finishedSlot(),
        9500
      ),
      "NotAssetOwner"
    );
  });

  it("refuses a second claim for the same asset and settlement slot", async () => {
    const owner = Keypair.generate();
    await fund(owner);
    const { assetId, asset } = await registerAsset("double-count-1", owner.publicKey);
    const periodStart = finishedSlot();
    const firstHash = sha256("double-count-1-first");

    await submitClaimIx(owner, assetId, asset, firstHash, periodStart, 9500).rpc(
      confirmed
    );

    // A different claim account, same asset and same half-hour: the Reading PDA
    // already exists, so the second submission cannot be created at all.
    let err: any;
    try {
      await submitClaimIx(
        owner,
        assetId,
        asset,
        sha256("double-count-1-second"),
        periodStart,
        9500
      ).rpc(confirmed);
    } catch (e) {
      err = e;
    }
    assert.exists(err, "expected the duplicate slot claim to fail");

    const reading = await program.account.reading.fetch(
      readingPda(assetId, periodStart)
    );
    assert.equal(
      reading.claim.toBase58(),
      claimPda(owner.publicKey, firstHash).toBase58()
    );
  });

  it("rejects periods that are unaligned or unfinished", async () => {
    const owner = Keypair.generate();
    await fund(owner);
    const { assetId, asset } = await registerAsset("period-1", owner.publicKey);

    await expectError(
      submitClaimIx(
        owner,
        assetId,
        asset,
        sha256("period-1-unaligned"),
        finishedSlot() + 7,
        9500
      ),
      "BadPeriod"
    );
    await expectError(
      submitClaimIx(
        owner,
        assetId,
        asset,
        sha256("period-1-future"),
        Math.floor(Date.now() / 1000 / SLOT_SECS) * SLOT_SECS + SLOT_SECS,
        9500
      ),
      "PeriodNotFinished"
    );
  });

  it("returns an unchallenged bond only after the challenge window closes", async () => {
    const submitter = Keypair.generate();
    const lateChallenger = Keypair.generate();
    await fund(submitter);
    await fund(lateChallenger);
    const { assetId, asset } = await registerAsset(
      "unchallenged-1",
      submitter.publicKey
    );
    const hash = sha256("unchallenged-1-claim");
    await submitClaimIx(
      submitter,
      assetId,
      asset,
      hash,
      finishedSlot(),
      9500
    ).rpc(confirmed);
    const claim = claimPda(submitter.publicKey, hash);

    await expectError(withdrawIx(submitter, claim), "ChallengeWindowOpen");
    await sleep((CHALLENGE_WINDOW_SECS + 5) * 1000);
    await expectError(
      challengeIx(lateChallenger, claim, hash),
      "ChallengeWindowClosed"
    );

    const claimLamports = async () =>
      (await connection.getAccountInfo(claim, "confirmed"))!.lamports;
    const before = await claimLamports();
    await withdrawIx(submitter, claim).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "finalized");
    assert.equal(claimAcc.bond.toNumber(), 0);
    assert.equal(before - (await claimLamports()), bond);
  });

  it("refunds both sides when the committee goes quiet", async () => {
    const { submitter, challenger, claim } = await disputedClaim("silent-1");

    await expectError(
      refundIx(claim, submitter.publicKey, challenger.publicKey),
      "ResolveWindowOpen"
    );
    await sleep((RESOLVE_WINDOW_SECS + 5) * 1000);

    const submitterBefore = await balance(submitter.publicKey);
    const challengerBefore = await balance(challenger.publicKey);
    await refundIx(claim, submitter.publicKey, challenger.publicKey).rpc(confirmed);

    assert.equal(await claimStatus(claim), "unresolved");
    assert.equal((await balance(submitter.publicKey)) - submitterBefore, bond);
    assert.equal((await balance(challenger.publicKey)) - challengerBefore, stake);
  });

  it("lets only the upgrade authority change the committee", async () => {
    const impostor = Keypair.generate();
    await fund(impostor);
    const newMember = Keypair.generate();
    const setResolversIx = (
      authority: Keypair | null,
      resolvers: PublicKey[],
      quorum: number
    ) => {
      const builder = program.methods
        .setResolvers(resolvers, quorum)
        .accountsPartial({
          authority: authority ? authority.publicKey : provider.wallet.publicKey,
          config,
          program: program.programId,
          programData,
        });
      return authority ? builder.signers([authority]) : builder;
    };
    const currentCommittee = async () =>
      (await program.account.config.fetch(config)).resolvers.map((r) =>
        r.toBase58()
      );

    await expectError(
      setResolversIx(impostor, [newMember.publicKey], 1),
      "Unauthorized"
    );

    // A quorum larger than the committee would strand every dispute.
    await expectError(
      setResolversIx(null, [newMember.publicKey], 2),
      "BadQuorum"
    );
    // So would a committee that lists the same member twice.
    await expectError(
      setResolversIx(null, [newMember.publicKey, newMember.publicKey], 2),
      "BadCommittee"
    );

    await setResolversIx(null, [newMember.publicKey], 1).rpc(confirmed);
    assert.deepEqual(await currentCommittee(), [newMember.publicKey.toBase58()]);

    // Put the original committee back so the suite stays order-independent.
    await setResolversIx(
      null,
      committee.map((m) => m.publicKey),
      QUORUM
    ).rpc(confirmed);
    assert.deepEqual(
      await currentCommittee(),
      committee.map((m) => m.publicKey.toBase58())
    );
  });
});
