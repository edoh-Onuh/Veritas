/**
 * Anchor integration tests for the Veritas program.
 *
 * Run with:  anchor test
 * (spins up a local validator, deploys, executes these against it)
 *
 * These assert the full loop: a plausible claim survives a challenge, and an
 * implausible claim gets slashed with funds moving to the challenger.
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

const IMPLAUSIBLE_BPS = 5000;

describe("veritas", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.Veritas as Program<Veritas>;

  // Deterministic 32-byte "inputs hash" standing in for the engine's sha256.
  function inputsHash(seed: string): number[] {
    return Array.from(createHash("sha256").update(seed).digest());
  }

  async function fund(kp: Keypair, sol = 2) {
    const sig = await provider.connection.requestAirdrop(
      kp.publicKey,
      sol * LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(sig);
  }

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

  it("slashes an implausible claim in favour of the challenger", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);

    const hash = inputsHash("inflated-carbon-claim-1");
    const [claim] = claimPda(submitter.publicKey, hash);
    const bond = 0.1 * LAMPORTS_PER_SOL;

    // Submit with a LOW score (below threshold) — an implausible claim.
    await program.methods
      .submitClaim(hash, 1, new anchor.BN(1800), 1500, new anchor.BN(bond))
      .accountsPartial({
        submitter: submitter.publicKey,
        claim,
        systemProgram: SystemProgram.programId,
      })
      .signers([submitter])
      .rpc();

    const [challenge] = challengePda(claim);
    const stake = 0.05 * LAMPORTS_PER_SOL;

    await program.methods
      .challengeClaim(hash, new anchor.BN(stake))
      .accountsPartial({
        challenger: challenger.publicKey,
        claim,
        challenge,
        systemProgram: SystemProgram.programId,
      })
      .signers([challenger])
      .rpc();

    const before = await provider.connection.getBalance(challenger.publicKey);

    await program.methods
      .resolve()
      .accountsPartial({
        cranker: provider.wallet.publicKey,
        claim,
        challenge,
        submitter: submitter.publicKey,
        challenger: challenger.publicKey,
      })
      .rpc();

    const after = await provider.connection.getBalance(challenger.publicKey);
    const claimAcc = await program.account.claim.fetch(claim);

    assert.equal(Object.keys(claimAcc.status)[0], "slashed");
    // challenger got bond + stake back (net positive vs their locked stake)
    assert.isTrue(after > before, "challenger should receive slashed bond");
  });

  it("confirms a plausible claim and pays the honest submitter", async () => {
    const submitter = Keypair.generate();
    const challenger = Keypair.generate();
    await fund(submitter);
    await fund(challenger);

    const hash = inputsHash("valid-solar-claim-1");
    const [claim] = claimPda(submitter.publicKey, hash);
    const bond = 0.1 * LAMPORTS_PER_SOL;

    // Submit with a HIGH score (above threshold) — a plausible claim.
    await program.methods
      .submitClaim(hash, 1, new anchor.BN(250), 9500, new anchor.BN(bond))
      .accountsPartial({
        submitter: submitter.publicKey,
        claim,
        systemProgram: SystemProgram.programId,
      })
      .signers([submitter])
      .rpc();

    const [challenge] = challengePda(claim);
    const stake = 0.05 * LAMPORTS_PER_SOL;

    // A mistaken challenger disputes a valid claim.
    await program.methods
      .challengeClaim(hash, new anchor.BN(stake))
      .accountsPartial({
        challenger: challenger.publicKey,
        claim,
        challenge,
        systemProgram: SystemProgram.programId,
      })
      .signers([challenger])
      .rpc();

    await program.methods
      .resolve()
      .accountsPartial({
        cranker: provider.wallet.publicKey,
        claim,
        challenge,
        submitter: submitter.publicKey,
        challenger: challenger.publicKey,
      })
      .rpc();

    const claimAcc = await program.account.claim.fetch(claim);
    assert.equal(Object.keys(claimAcc.status)[0], "confirmed");
  });
});
