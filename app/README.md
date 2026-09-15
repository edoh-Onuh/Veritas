# app/

## demo.html — the pitch demo (zero setup)

Open `demo.html` in any browser. No build step, no server, no wallet needed.
Pick a case, hit **Run verification**, and watch the full loop: physics checks
resolve against live-grid ground truth, a verdict lands, and the on-chain
sequence runs submit → challenge → resolve with devnet explorer links.

The three cases mirror the Python engine's exact output:
- **case 01 — Plausible claim**: passes all checks, confirmed on-chain.
- **case 02 — Impossible energy**: 128% capacity factor, slashed.
- **case 03 — Inflated carbon**: plausible energy but false CO₂ caught against
  live grid intensity, slashed. *This is the money shot for the video.*

For the pitch video, record case 03. It's the one no centralized MRV box can
replicate, because the ceiling it fails against moves with real-time grid data.

### Wiring to real devnet

Verdicts are real (from the engine); the on-chain signatures are simulated so
the demo runs anywhere with zero setup. To connect real Anchor calls, replace
the three marked (★) blocks in the `<script>` with:

```
program.methods.submitClaim(hash, version, claimedCo2, scoreBps, bond).rpc()
program.methods.challengeClaim(hash, stake).rpc()
program.methods.resolve().rpc()
```

using the client pattern in `src/veritas.test.ts`, and swap `GRID` for a live
fetch from the engine's `/score` output.

## veritas.test.ts — Anchor integration tests

Run with `anchor test` from the repo root (see top-level README).
