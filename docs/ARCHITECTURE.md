# Architecture

## One product, two halves that need each other

Neither half is a startup alone. A Python script that scores claims isn't verifiable — you'd just be trusting a new box. An empty slashing contract verifies nothing — it has no notion of what makes a claim true. Together they're a plausibility oracle: physical truth, made portable and hard to fake.

```
                    off-chain                         on-chain (Solana)
   ┌─────────────────────────────────┐       ┌──────────────────────────────┐
   │  Veritas engine (Python)        │       │  Veritas program (Anchor)    │
   │                                 │       │                              │
   │  claim ──► 5 physics checks ──► │  hash │  submit_claim   (commit)     │
   │           live NESO grid data   │ ────► │  challenge_claim(dispute)    │
   │           ► verdict + score     │ score │  resolve        (slash/keep) │
   │           ► inputs_hash (sha256)│       │                              │
   └─────────────────────────────────┘       │  ► portable integrity score  │
                                              └──────────────┬───────────────┘
                                                             │ account read / CPI
                                              ┌──────────────▼───────────────┐
                                              │  any consumer: carbon market, │
                                              │  RWA protocol, DPP registry   │
                                              └───────────────────────────────┘
```

## Why the commitment is a hash, not the full claim

Only `inputs_hash` (sha256 of the canonically-serialized claim) plus headline figures go on-chain. The full claim — asset details, energy, period — stays off-chain. This keeps per-attestation cost near zero (essential when you're attesting every half-hour) while still binding the on-chain record to exact off-chain inputs: at resolution, a challenger's independently-recomputed hash must equal the committed one, proving both parties scored the *same* claim.

## Why resolution is deterministic

The physics engine is a pure function: same inputs → same verdict, every time. Anyone can re-run the engine on the committed inputs and get the same score, so "was this claim implausible?" has one reproducible answer. That determinism is what makes a committee work: `submit_resolution` records each member's re-derived score, and the claim settles only when a quorum reports the *same* number — honest members converge because the function is deterministic, and a member who reports something else is visibly out of step with anyone who re-runs it. The hardened version re-derives the score from inputs on-chain, so no quorum has to be trusted at all (see roadmap).

## The optimistic model

Veritas assumes claims are valid and makes disputing them cheap and rewarding — the same design logic as optimistic rollups. A submitter stakes a bond; a challenger stakes to dispute it within the challenge window; the loser's stake pays the winner. Fabrication is unprofitable in expectation as long as honest challengers watch the stream and a quorum of the committee reports the score it actually derived.

A claim is not a free-floating assertion: it names a registered asset and one half-hourly settlement slot. The asset registry holds the capacity, region and latitude the claim is judged against, so a submitter cannot invent a 1 GW farm; only the asset's registered owner can claim its output; and a `Reading` account seeded by asset and slot means the same half-hour cannot be sold twice.

Every escrow has a way out. An unchallenged bond is withdrawable by its submitter once the challenge window closes; a confirmed claim returns the bond with the challenger's stake; and a challenge the resolver leaves past its deadline can be refunded to both sides by anyone. This is deliberately simple for a four-week build and deliberately honest about it: the rest of the cryptoeconomic hardening (bond sizing, minimum stakes, griefing resistance) is roadmap, not claimed as done.

## Data source

UK Carbon Intensity API (NESO, in partnership with EDF Europe, Oxford, WWF). Keyless, CC BY 4.0, half-hourly, with `actual` and `forecast` intensity in gCO₂/kWh, an 18-region breakdown, and generation mix. The engine uses the regional endpoint keyed by DNO region id, with a bundled fallback for API downtime.

## Extending to new methodologies

Each methodology is a set of physics checks plus a ground-truth data source. Grid energy uses capacity/irradiance bounds + live grid intensity. Biochar would use a carbon mass-balance check (carbon out ≤ carbon in feedstock) + methodology parameters. Reforestation would use biomass-from-imagery bounds + satellite data. The commit-score-challenge loop is identical; only the check set and data source change.
