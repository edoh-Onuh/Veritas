/**
 * Anchor integration tests for the Veritas program.
 *
 * Run with:  anchor test
 * (spins up a local validator, deploys, executes these against it)
 *
 * These assert the full loop: the resolver's score, not the submitter's, decides
 * a dispute; only the configured resolver can settle one; and no bond or stake
 * stays locked, whether a claim goes unchallenged or the resolver goes quiet.
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
const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

describe("veritas", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.Veritas as Program<Veritas>;
  const connection = provider.connection;

  const resolver = Keypair.generate();
  const [config] = PublicKey.findProgramAddressSync(
    [Buffer.from("config")],
    program.programId
  );
  const bond = 0.1 * LAMPORTS_PER_SOL;
  const stake = 0.05 * LAMPORTS_PER_SOL;
  const confirmed = { commitment: "confirmed" as const };

  // Deterministic 32-byte "inputs hash" standing in for the engine's sha256.
  function inputsHash(seed: string): number[] {
    return Array.from(createHash("sha256").update(seed).digest());
  }

  async function fund(kp: Keypair, sol = 2) {
    const sig = await connection.requestAirdrop(
      kp.publicKey,
      sol * LAMPORTS_PER_SOL
    );
    await connection.confirmTransaction(sig, "confirmed");
  }

  const balance = (key: PublicKey) => connection.getBalance(key, "confirmed");
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  function claimPda(submitter: PublicKey, hash: number[]): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("claim"), submitter.toBuffer(), Buffer.from(hash)],
      program.programId
    );
  }

  function challengePda(claim: PublicKey): [PublicKey, number] {
    return PublicKey.findProgramAddressSync(
      [Buffer.from("challenge"), claim.toBuffer()],
      program.programId
    );
  }

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

  function submitClaimIx(
    submitter: Keypair,
    hash: number[],
    claim: PublicKey,
    scoreBps: number
  ) {
    return program.methods
      .submitClaim(hash, 1, new anchor.BN(1800), scoreBps, new anchor.BN(bond))
      .accountsPartial({
        submitter: submitter.publicKey,
        claim,
        systemProgram: SystemProgram.programId,
      })
      .signers([submitter]);
  }

  function challengeIx(
    challenger: Keypair,
    claim: PublicKey,
    challenge: PublicKey,
    hash: number[]
  ) {
    return program.methods
      .challengeClaim(hash, new anchor.BN(stake))
      .accountsPartial({
        challenger: challenger.publicKey,
        config,
        claim,
        challenge,
        systemProgram: SystemProgram.programId,
      })
      .signers([challenger]);
  }

  function resolveIx(
    signer: Keypair,
    scoreBps: number,
    claim: PublicKey,
    challenge: PublicKey,
    submitter: PublicKey,
    challenger: PublicKey
  ) {
    return program.methods
      .resolve(scoreBps)
      .accountsPartial({
        resolver: signer.publicKey,
        config,
        claim,
        challenge,
        submitter,
        challenger,
      })
      .signers([signer]);
  }

  function withdrawIx(submitter: Keypair, claim: PublicKey) {
    return program.methods
      .withdrawBond()
      .accountsPartial({ submitter: submitter.publicKey, config, claim })
      .signers([submitter]);
  }

  function refundIx(
    claim: PublicKey,
    challenge: PublicKey,
    submitter: PublicKey,
    challenger: PublicKey
  ) {
    return program.methods.refundExpiredChallenge().accountsPartial({
      cranker: provider.wallet.publicKey,
      config,
      claim,
      challenge,
      submitter,
      challenger,
    });
  }

  async function submitClaim(
    submitter: Keypair,
    seed: string,
    scoreBps: number
  ) {
    const hash = inputsHash(seed);
    const [claim] = claimPda(submitter.publicKey, hash);
    await submitClaimIx(submitter, hash, claim, scoreBps).rpc(confirmed);
    return { hash, claim };
  }

  async function challengeClaim(
    challenger: Keypair,
    claim: PublicKey,
    hash: number[]
  ) {
    const [challenge] = challengePda(claim);
    await challengeIx(challenger, claim, challenge, hash).rpc(confirmed);
    return challenge;
  }

  before(async () => {
    await fund(resolver);
    const [programData] = PublicKey.findProgramAddressSync(
      [program.programId.toBuffer()],
      BPF_LOADER_UPGRADEABLE
    );
    // The provider wallet deployed the program, so it is the upgrade authority.
    await program.methods
      .initializeConfig(
        resolver.publicKey,
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

  it("slashes a claim the resolver scores as implausible, whatever score the submitter committed", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);

    // The submitter commits a HIGH score for an inflated claim.
    const { hash, claim } = await submitClaim(
      submitter,
      "inflated-carbon-claim-1",
      9500
    );
    const challenge = await challengeClaim(challenger, claim, hash);

    const before = await balance(challenger.publicKey);
    await resolveIx(
      resolver,
      1500,
      claim,
      challenge,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "slashed");
    assert.equal(claimAcc.resolvedScoreBps, 1500);
    assert.equal((await balance(challenger.publicKey)) - before, bond + stake);
  });

  it("confirms a claim the resolver scores as plausible and pays the submitter bond plus stake", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);

    // A low self-score does not doom a claim either; only the resolver's counts.
    const { hash, claim } = await submitClaim(
      submitter,
      "valid-solar-claim-1",
      1500
    );
    const challenge = await challengeClaim(challenger, claim, hash);

    const before = await balance(submitter.publicKey);
    await resolveIx(
      resolver,
      9500,
      claim,
      challenge,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "confirmed");
    assert.equal(claimAcc.resolvedScoreBps, 9500);
    assert.equal(claimAcc.bond.toNumber(), 0);
    assert.equal((await balance(submitter.publicKey)) - before, bond + stake);
  });

  it("rejects resolution signed by anyone other than the configured resolver", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    const impostor = Keypair.generate();
    await fund(submitter);
    await fund(challenger);
    await fund(impostor);

    const { hash, claim } = await submitClaim(
      submitter,
      "impostor-resolution-1",
      10000
    );
    const challenge = await challengeClaim(challenger, claim, hash);

    await expectError(
      resolveIx(
        impostor,
        9500,
        claim,
        challenge,
        submitter.publicKey,
        challenger.publicKey
      ),
      "Unauthorized"
    );
    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "challenged");
  });

  it("returns an unchallenged bond only after the challenge window closes", async () => {
    const submitter = Keypair.generate();
    const lateChallenger = Keypair.generate();
    await fund(submitter);
    await fund(lateChallenger);

    const { hash, claim } = await submitClaim(
      submitter,
      "unchallenged-claim-1",
      9500
    );
    const [challenge] = challengePda(claim);

    await expectError(withdrawIx(submitter, claim), "ChallengeWindowOpen");
    await sleep((CHALLENGE_WINDOW_SECS + 5) * 1000);
    await expectError(
      challengeIx(lateChallenger, claim, challenge, hash),
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

  it("refunds both sides when the resolver misses its deadline", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);

    const { hash, claim } = await submitClaim(
      submitter,
      "resolver-timeout-1",
      9500
    );
    const challenge = await challengeClaim(challenger, claim, hash);

    await expectError(
      refundIx(claim, challenge, submitter.publicKey, challenger.publicKey),
      "ResolveWindowOpen"
    );
    await sleep((RESOLVE_WINDOW_SECS + 5) * 1000);

    const submitterBefore = await balance(submitter.publicKey);
    const challengerBefore = await balance(challenger.publicKey);
    await refundIx(
      claim,
      challenge,
      submitter.publicKey,
      challenger.publicKey
    ).rpc(confirmed);

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "unresolved");
    assert.equal((await balance(submitter.publicKey)) - submitterBefore, bond);
    assert.equal((await balance(challenger.publicKey)) - challengerBefore, stake);
  });

  it("lets only the upgrade authority rotate the resolver", async () => {
    const impostor = Keypair.generate();
    await fund(impostor);
    const nextResolver = Keypair.generate();
    const [programData] = PublicKey.findProgramAddressSync(
      [program.programId.toBuffer()],
      BPF_LOADER_UPGRADEABLE
    );
    // `null` signs with the provider wallet, which is the upgrade authority.
    const setResolverIx = (authority: Keypair | null, next: PublicKey) => {
      const builder = program.methods.setResolver(next).accountsPartial({
        authority: authority ? authority.publicKey : provider.wallet.publicKey,
        config,
        program: program.programId,
        programData,
      });
      return authority ? builder.signers([authority]) : builder;
    };
    const currentResolver = async () =>
      (await program.account.config.fetch(config)).resolver.toBase58();

    await expectError(
      setResolverIx(impostor, nextResolver.publicKey),
      "Unauthorized"
    );
    assert.equal(await currentResolver(), resolver.publicKey.toBase58());

    await setResolverIx(null, nextResolver.publicKey).rpc(confirmed);
    assert.equal(await currentResolver(), nextResolver.publicKey.toBase58());

    // Put the original resolver back so the suite stays order-independent.
    await setResolverIx(null, resolver.publicKey).rpc(confirmed);
    assert.equal(await currentResolver(), resolver.publicKey.toBase58());
  });
});
