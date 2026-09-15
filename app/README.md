# app/

## demo.html — the pitch demo (zero setup)

Open `demo.html` in any browser. No build step, no server, no wallet needed.
Pick a case, hit **Run verification**, and watch the full loop: physics checks
resolve against live-grid ground truth, a verdict lands, and the on-chain
sequence runs submit → challenge → resolve with devnet explorer links.

The three cases are a **recorded run** of the Python engine against a grid at
180 gCO₂/kWh, not a live one:
- **case 01 — Plausible claim**: passes all checks, confirmed on-chain.
- **case 02 — Impossible energy**: 128% capacity factor, slashed.
- **case 03 — Inflated carbon**: plausible energy but false CO₂ caught against
  grid intensity, slashed. *This is the money shot for the video.*

For the pitch video, record case 03. It's the one no centralized MRV box can
replicate, because the ceiling it fails against moves with real-time grid data.

That movement is also why these figures are a recording: run
`python -m veritas` and the engine sizes the same three claims against the sun
that was actually up and the intensity the grid was actually running at, so its
numbers will differ from the ones frozen here — a claim of 250 kg is plausible
at 180 gCO₂/kWh and impossible at 26.

### Wiring to real devnet

Verdicts are real (from the engine); the on-chain signatures are simulated so
the demo runs anywhere with zero setup. To connect real Anchor calls, replace
the three marked (★) blocks in the `<script>` with:

```
// once per asset, by the program's upgrade authority:
program.methods.registerAsset(assetId, owner, assetType, capacityKw, regionId, latMillideg).rpc()

program.methods.submitClaim(assetId, hash, modelVersion, periodStart, claimedCo2, scoreBps, bond).rpc()
program.methods.challengeClaim(hash, stake).rpc()
program.methods.submitResolution(scoreBps).rpc()   // one per committee member, until quorum
```

using the client pattern in `src/veritas.test.ts`, and swap `GRID` for a live
fetch from the engine's `/score` output. Note that `submitClaim` must be signed
by the asset's registered owner, and that each (asset, settlement slot) can be
claimed only once.

## veritas.test.ts — Anchor integration tests

Run with `anchor test` from the repo root (see top-level README).
