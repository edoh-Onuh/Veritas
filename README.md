# Veritas

**A plausibility oracle for physical claims.** Physics-informed models score whether reported energy and emissions data is physically possible, then commit and challenge that verdict on Solana — so any market can consume a portable integrity score instead of trusting a closed box.

Built for Colosseum's Crypto World's Fair (Sept 14 – Oct 12, 2026). Tracks: Infrastructure, general pool, Solana.

---

## The problem

The voluntary carbon market lost 50–90% of its credibility between 2023–24 when centralized verifiers were caught approving over-credited and fabricated projects, triggering a ~61% market contraction. Every existing digital MRV platform is a closed Web2 box that *asserts* trust. None puts the verification logic — the model that decides whether a reported number is physically possible — into an open, economically-secured, independently-challengeable protocol.

That verification layer is the open lane. Veritas builds it.

## The insight

Most fraud isn't a broken database — it's a number that violates physics. A solar farm can't deliver more energy than its nameplate capacity. It can't produce at midnight. You can't avoid more CO₂ than the grid would have emitted producing that same energy. Veritas checks claims against these bounds using **live grid data** and returns a verdict that names the exact law each claim breaks.

The core check moves with the real world: the same 250 kg CO₂ claim is plausible when the grid is dirty (180 gCO₂/kWh) and impossible when the grid is clean (42 gCO₂/kWh), because adding clean energy to an already-clean grid displaces little carbon. No static rulebook and no centralized MRV box can do this — it needs live physical data and a physics bound. That's the moat.

## How it works

Three roles, one optimistic verification loop:

1. **Submitter** commits a physics-scored claim on Solana — only its hash and headline figures go on-chain (cheap); the full claim stays off-chain. A bond is staked.
2. **Plausibility engine** scores the claim against five physics checks (below), each naming the law it enforces, using live NESO grid intensity.
3. **Challenger** stakes to dispute a claim while its challenge window is open. A committee of independent re-runners each re-runs the engine on the committed inputs and submits the score it derives; the claim settles when a quorum reports the *same* score. An implausible claim gets slashed, the challenger is rewarded, and a portable integrity score is written for other programs to read. A claim nobody disputes returns its bond once the window closes.

**Why Solana:** attestations are per-reading and high-frequency (half-hourly settlement slots, per batch, per shipment), and the challenge game needs cheap, fast finality. At Ethereum L1 gas prices a single attestation costs more than the data point is worth. On Solana it's fractions of a cent. The economics only close on a high-throughput, low-fee chain.

## The five physics checks

| Check | Physical law | Catches |
|---|---|---|
| Capacity ceiling | conservation of energy (output ≤ capacity × time) | claims exceeding 100% capacity factor |
| Resource envelope | source-specific resource limit (solar irradiance) | solar output at night, above clear-sky ceiling |
| Avoided-emissions bound | avoided CO₂ ≤ energy × live grid intensity | inflated carbon against real-time grid |
| Internal consistency | dimensional consistency | unit errors (kg/tonne 1000× mixups) |
| Temporal validity | measurement post-dates the period | forecasts passed off as measurements |

---

## Repo layout

```
veritas/
├── engine/                 # Python physics engine (the scoring core)
│   ├── veritas/engine.py   # checks, live NESO client, scoring
│   ├── tests/              # 16 deterministic tests (offline)
│   └── pyproject.toml
├── programs/veritas/       # Anchor on-chain program (Rust)
│   └── src/lib.rs          # submit_claim / challenge_claim / resolve
├── app/src/                # TypeScript integration tests + client
│   └── veritas.test.ts     # full submit→challenge→slash loop
├── scripts/
│   └── submit_claim.py     # bridge: engine verdict → on-chain args
├── docs/ARCHITECTURE.md
├── Anchor.toml
└── Cargo.toml
```

## Run it

### Engine (verified — runs anywhere with Python 3.10+)

```bash
cd engine
pip install -r requirements.txt
python -m veritas.engine          # prints the 3 demo verdicts
python -m pytest tests/ -q        # 16 tests, all offline & deterministic
```

The engine calls the live UK Carbon Intensity API (`api.carbonintensity.org.uk`, keyless, CC BY 4.0). If that slot's data is unreachable the engine returns `UNVERIFIED` rather than scoring the claim against a guessed intensity: a submitter who can force a fallback could otherwise pick their own carbon ceiling. `UNVERIFIED` scores 0.50, at the on-chain implausibility threshold, so it can never be certified.

Before any bound is tested, an input-validity gate rejects figures that are not physical quantities — NaN, infinity, negative energy or CO₂, non-positive capacity, latitude outside ±90, unknown DNO regions, unparseable timestamps. NaN compares false against every ceiling, so without that gate a NaN claim passes all five checks.

### Bridge (verified)

```bash
python scripts/submit_claim.py --demo inflated --dry-run
```

Scores a claim and prints the exact `submit_claim` instruction args (inputs_hash, score in basis points, bond) that the on-chain program receives.

### On-chain program (build locally)

Requires Linux/WSL with Solana CLI `1.18.26`, Anchor CLI `0.30.1`, and Rust `nightly-2025-04-10` (for IDL generation only):

```bash
rustup toolchain install nightly-2025-04-10 --profile minimal
export RUSTUP_TOOLCHAIN=nightly-2025-04-10   # Anchor 0.30.1 builds the IDL with +nightly;
                                             # newer nightlies removed proc_macro::SourceFile
anchor build
anchor test --provider.cluster localnet      # local validator: submit → challenge → resolve
# WSL1 only: Anchor's port check wrongly reports 8899 as in use, so start the validator yourself:
#   solana-test-validator --reset --quiet &
#   anchor test --skip-local-validator --provider.cluster localnet
anchor deploy --provider.cluster devnet
```

The program is deployed on devnet at `DypSeezrbcEhDSJNganfpjjkQXp1NBAHpvDAQrQBHLEW`, with its `Config` PDA at
`7ygKX1VDHgseUSggX23tcQLvx8MUVhX4g3geDqR2K582`: a one-member committee (`3HdgSu5vgpAq7MbZmLRtraQ6ZhCyzw2trfaK8CQtgUYP`,
quorum 1) with a 24h challenge window and a 72h resolution deadline. One member is the weakest a committee can be —
`set_resolvers` adds independent re-runners without redeploying. A demo asset is registered at
`BWfN3N83CM8HT8X7p9QKaFp8To7ZazUcZ4BrebU9TdAz` (5 MW solar, DNO region 3, lat 53.5): 5 MW solar, asset id
`01a27315adfb575973486b4afa1d42b2b286c274ead2762b6cd65ab19807bc80`.

Building your own deployment? Run `anchor keys list`, put your id in `declare_id!` and `Anchor.toml`, then call
`initialize_config` once, and `register_asset` for each asset, before any claim can be submitted or challenged.
`initialize_config`, `set_resolvers` and `register_asset` are all gated on the program's upgrade authority.

`Cargo.lock` is resolved for the Rust 1.75 compiler inside Solana 1.18's platform-tools, with `blake3 1.8.2`, `jobserver 0.1.32` and `proc-macro2 1.0.94` pinned. Don't `cargo update` it.

> **Verified vs. build-locally.** The Python engine and its 16 tests, and the bridge script, are tested and passing in this repo. The Rust program and TypeScript tests are written against the Anchor 0.30.1 API but must be compiled with the Solana/Anchor toolchain on your machine — those toolchains aren't installable in the sandbox this was authored in. Build them locally before the demo.

## Honest limitations (and the roadmap they imply)

These are stated plainly because scoped honesty is a strength, not a gap — and each names a real research direction:

- **A committee, not a proof, at MVP.** A dispute is settled when a quorum of the configured committee reports the same re-derived score; the submitter's own score is recorded but never decides the outcome. That moves the trust from the submitter (who profits from lying) to a set of parties whose work anyone can reproduce by re-running the engine — but a colluding quorum is still trusted, and the committee is appointed by the program's upgrade authority. Making the *computation itself* trustlessly verifiable on-chain — via verifiable compute — is the core post-hackathon problem, and the one this project's physics-informed background is built for.
- **Liveness, not custody, is the committee's power.** It cannot take anyone's money: funds only ever go to the submitter or the challenger. If it goes quiet, anyone can refund both sides once the deadline passes; if its members disagree, the split refunds both sides too. No bond or stake can be held hostage.
- **Input attestation is a later layer.** Claims are pinned to a registered asset — its capacity, region and latitude come from the registry, only its owner can claim its output, and each half-hour can be claimed once — but the meter reading inside that envelope is still the submitter's word. Catching fabricated raw inputs (fake sensor data) is a sensor-attestation / DePIN problem layered underneath this one, and the registry is what gives it something to attach to.
- **Solar-only resource envelope.** Wind, hydro, and battery envelopes are stubbed as roadmap; the framework generalizes to each.
- **One methodology.** Grid energy is the beachhead. The same commit-score-challenge loop extends to any physical claim — biochar mass balance, reforestation remote sensing, EU Digital Product Passport embedded carbon.

## Author

John Edoh Onuh — physics (BSc), data science (MSc), physics-informed ML research, and a circular-economy / environmental-data background where unverifiable claims are a daily problem. The plausibility model is the actual overlap of that work.
