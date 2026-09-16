#!/usr/bin/env node
/**
 * Send a scored claim to the Veritas program.
 *
 * The Python bridge scores a claim and emits the exact instruction arguments;
 * this sends them. Keeping the scorer and the sender apart means the engine
 * never needs a funded keypair, and this never needs to know any physics.
 *
 *   python scripts/submit_claim.py --demo good --json > /tmp/claim.json
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=~/.config/solana/id.json \
 *     node scripts/submit_onchain.js submit /tmp/claim.json
 *
 * Other subcommands act on a claim that already exists on-chain:
 *   node scripts/submit_onchain.js challenge /tmp/claim.json [stakeLamports]
 *   node scripts/submit_onchain.js resolve   /tmp/claim.json <scoreBps>
 *   node scripts/submit_onchain.js show      /tmp/claim.json
 *
 * The wallet must be the asset's registered owner to submit, and a member of
 * the committee to resolve.
 */

const fs = require("fs");
const path = require("path");
const anchor = require("@coral-xyz/anchor");
const { PublicKey, SystemProgram } = require("@solana/web3.js");

const IDL_PATH =
  process.env.VERITAS_IDL ||
  path.join(__dirname, "..", "target", "idl", "veritas.json");

const hexBytes = (hex) => Array.from(Buffer.from(hex, "hex"));

function explorer(sig, rpc) {
  const cluster = rpc.includes("devnet")
    ? "?cluster=devnet"
    : rpc.includes("127.0.0.1") || rpc.includes("localhost")
    ? "?cluster=custom"
    : "";
  return `https://explorer.solana.com/tx/${sig}${cluster}`;
}

function pdas(program, claimArgs, submitter) {
  const assetId = hexBytes(claimArgs.asset_id);
  const hash = hexBytes(claimArgs.inputs_hash);
  const [config] = PublicKey.findProgramAddressSync(
    [Buffer.from("config-v2")],
    program.programId
  );
  const [asset] = PublicKey.findProgramAddressSync(
    [Buffer.from("asset"), Buffer.from(assetId)],
    program.programId
  );
  const [reading] = PublicKey.findProgramAddressSync(
    [
      Buffer.from("reading"),
      Buffer.from(assetId),
      new anchor.BN(claimArgs.period_start).toArrayLike(Buffer, "le", 8),
    ],
    program.programId
  );
  const [claim] = PublicKey.findProgramAddressSync(
    [Buffer.from("claim"), submitter.toBuffer(), Buffer.from(hash)],
    program.programId
  );
  const [challenge] = PublicKey.findProgramAddressSync(
    [Buffer.from("challenge"), claim.toBuffer()],
    program.programId
  );
  return { assetId, hash, config, asset, reading, claim, challenge };
}

async function main() {
  const [command, argsPath, extra] = process.argv.slice(2);
  const CLAIM_COMMANDS = ["submit", "challenge", "resolve", "show"];
  if (!command || (CLAIM_COMMANDS.includes(command) && !argsPath)) {
    console.error(
      "usage: submit_onchain.js <submit|challenge|resolve|show> <claim.json> [stake|scoreBps]\n" +
        "       submit_onchain.js config\n" +
        "       submit_onchain.js committee <pubkey,pubkey,...> <quorum>"
    );
    process.exit(2);
  }

  let claimArgs = null;
  if (CLAIM_COMMANDS.includes(command)) {
    claimArgs = JSON.parse(fs.readFileSync(argsPath, "utf8"));
    if (!claimArgs.inputs_hash) {
      console.error(
        "this claim has no inputs_hash, so the engine refused to score it:",
        claimArgs.verdict
      );
      process.exit(1);
    }
  }

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const idl = JSON.parse(fs.readFileSync(IDL_PATH, "utf8"));
  const program = new anchor.Program(idl, provider);
  const wallet = provider.wallet.publicKey;
  const rpc = provider.connection.rpcEndpoint;
  const confirmed = { commitment: "confirmed" };
  const [configPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("config-v2")],
    program.programId
  );
  const [programData] = PublicKey.findProgramAddressSync(
    [program.programId.toBuffer()],
    new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111")
  );

  console.log(`program   : ${program.programId.toBase58()}`);
  console.log(`rpc       : ${rpc}`);
  console.log(`wallet    : ${wallet.toBase58()}`);

  if (command === "config") {
    const cfg = await program.account.config.fetchNullable(configPda);
    if (!cfg) return console.log("config: not initialised on this cluster");
    console.log("config    :", configPda.toBase58());
    console.log("committee :", cfg.resolvers.map((r) => r.toBase58()).join(", "));
    console.log("quorum    :", `${cfg.quorum} of ${cfg.resolvers.length}`);
    console.log(
      "windows   :",
      `challenge ${cfg.challengeWindowSecs}s, resolve ${cfg.resolveWindowSecs}s`
    );
    console.log(
      "balance   :",
      await provider.connection.getBalance(configPda, "confirmed"),
      "lamports (rent + the protocol's share of slashed bonds)"
    );
    return;
  }

  if (command === "committee") {
    if (!argsPath || extra === undefined) {
      console.error(
        "committee needs members and a quorum: committee <pubkey,pubkey,...> <quorum>"
      );
      process.exit(2);
    }
    const members = argsPath.split(",").map((s) => new PublicKey(s.trim()));
    const quorum = Number(extra);
    // The program rejects a quorum larger than the committee, duplicate
    // members, and anyone who is not the upgrade authority.
    const sig = await program.methods
      .setResolvers(members, quorum)
      .accountsPartial({
        authority: wallet,
        config: configPda,
        program: program.programId,
        programData,
      })
      .rpc(confirmed);
    console.log(`committee : ${members.length} members, quorum ${quorum}`);
    console.log("signature :", sig);
    console.log("explorer  :", explorer(sig, rpc));
    return;
  }

  const p = pdas(program, claimArgs, wallet);
  console.log(`claim PDA : ${p.claim.toBase58()}`);

  if (command === "show") {
    const account = await program.account.claim.fetchNullable(p.claim);
    if (!account) return console.log("claim: not on chain yet");
    console.log("status    :", Object.keys(account.status)[0]);
    console.log("submitter :", account.submitter.toBase58());
    console.log("asset     :", account.asset.toBase58());
    console.log("period    :", account.periodStart.toString());
    console.log("committed :", account.integrityScoreBps, "bps (self-reported)");
    console.log(
      "resolved  :",
      account.resolvedScoreBps === null
        ? "not yet"
        : `${account.resolvedScoreBps} bps (committee)`
    );
    console.log("bond      :", account.bond.toString(), "lamports");
    return;
  }

  if (command === "submit") {
    console.log(
      `verdict   : ${claimArgs.verdict} (${claimArgs.integrity_score_bps} bps) ` +
        `at ${claimArgs.grid_intensity_gco2_kwh} gCO2/kWh`
    );
    const sig = await program.methods
      .submitClaim(
        p.assetId,
        p.hash,
        claimArgs.model_version,
        new anchor.BN(claimArgs.period_start),
        new anchor.BN(claimArgs.claimed_co2_kg),
        claimArgs.integrity_score_bps,
        new anchor.BN(claimArgs.bond_lamports)
      )
      .accountsPartial({
        submitter: wallet,
        asset: p.asset,
        reading: p.reading,
        claim: p.claim,
        systemProgram: SystemProgram.programId,
      })
      .rpc(confirmed);
    console.log("submitted :", sig);
    console.log("explorer  :", explorer(sig, rpc));
    return;
  }

  if (command === "challenge") {
    const stake = Number(extra ?? Math.floor(claimArgs.bond_lamports / 5));
    const sig = await program.methods
      .challengeClaim(p.hash, new anchor.BN(stake))
      .accountsPartial({
        challenger: wallet,
        config: p.config,
        claim: p.claim,
        challenge: p.challenge,
        systemProgram: SystemProgram.programId,
      })
      .rpc(confirmed);
    console.log(`challenged: ${sig} (stake ${stake} lamports)`);
    console.log("explorer  :", explorer(sig, rpc));
    return;
  }

  if (command === "resolve") {
    if (extra === undefined) {
      console.error("resolve needs the score in basis points, e.g. 1500");
      process.exit(2);
    }
    const account = await program.account.claim.fetch(p.claim);
    const sig = await program.methods
      .submitResolution(Number(extra))
      .accountsPartial({
        resolver: wallet,
        config: p.config,
        claim: p.claim,
        challenge: p.challenge,
        submitter: account.submitter,
        challenger: account.challenger,
      })
      .rpc(confirmed);
    console.log(`resolution: ${sig} (${extra} bps)`);
    console.log("explorer  :", explorer(sig, rpc));
    const after = await program.account.claim.fetch(p.claim);
    console.log("status    :", Object.keys(after.status)[0]);
    return;
  }

  console.error(`unknown command: ${command}`);
  process.exit(2);
}

main().catch((e) => {
  console.error("failed:", e.message);
  process.exit(1);
});
