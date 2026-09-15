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
3. **Challenger** stakes to dispute a claim. Because the engine is deterministic, resolution is reproducible: an implausible claim gets slashed, the challenger is rewarded, and a portable integrity score is written for other programs to read.

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

The engine calls the live UK Carbon Intensity API (`api.carbonintensity.org.uk`, keyless, CC BY 4.0). If the network is unreachable it degrades to a bundled fallback value so a live demo never breaks — the NESO docs warn the API can be slow.

### Bridge (verified)

```bash
python scripts/submit_claim.py --demo inflated --dry-run
```

Scores a claim and prints the exact `submit_claim` instruction args (inputs_hash, score in basis points, bond) that the on-chain program receives.

### On-chain program (build locally)

Requires the Solana + Anchor toolchain (`anchor 0.30.1`, `solana` CLI):

```bash
anchor build
anchor keys list                  # copy the program id
# paste it into declare_id! in programs/veritas/src/lib.rs and Anchor.toml
anchor build
anchor test                       # local validator: submit → challenge → resolve
anchor deploy --provider.cluster devnet
```

> **Verified vs. build-locally.** The Python engine and its 16 tests, and the bridge script, are tested and passing in this repo. The Rust program and TypeScript tests are written against the Anchor 0.30.1 API but must be compiled with the Solana/Anchor toolchain on your machine — those toolchains aren't installable in the sandbox this was authored in. Build them locally before the demo.

## Honest limitations (and the roadmap they imply)

These are stated plainly because scoped honesty is a strength, not a gap — and each names a real research direction:

- **Optimistic trust at MVP.** `resolve` trusts that the integrity score committed at submit time was correctly derived from the committed inputs. Making the *computation itself* trustlessly verifiable on-chain — via verifiable compute, or a committee of independent re-runners — is the core post-hackathon problem, and the one this project's physics-informed background is built for.
- **Input attestation is a later layer.** The engine trusts the submitter's meter figures. Catching a submitter who fabricates the raw inputs (fake sensor data) is a sensor-attestation / DePIN problem layered underneath this one.
- **Solar-only resource envelope.** Wind, hydro, and battery envelopes are stubbed as roadmap; the framework generalizes to each.
- **One methodology.** Grid energy is the beachhead. The same commit-score-challenge loop extends to any physical claim — biochar mass balance, reforestation remote sensing, EU Digital Product Passport embedded carbon.

## Author

John Edoh Onuh — physics (BSc), data science (MSc), physics-informed ML research, and a circular-economy / environmental-data background where unverifiable claims are a daily problem. The plausibility model is the actual overlap of that work.
