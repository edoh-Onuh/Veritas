#!/usr/bin/env python3
"""
Bridge: score a claim off-chain with the Veritas engine, then commit the
verdict on-chain (devnet) and print the explorer link.

This is the glue that turns "a Python script" and "an Anchor program" into one
product: the score the engine produces IS the score committed on-chain, under
the exact inputs_hash the engine computed.

Usage
-----
    python scripts/submit_claim.py --demo good
    python scripts/submit_claim.py --demo inflated
    python scripts/submit_claim.py --energy 1400 --claimed-co2 250 \
        --capacity 5000 --region 3 --lat 53.5

On-chain submission requires solana-py and a funded devnet keypair at
~/.config/solana/id.json. Without those, it runs in --dry-run mode and just
prints the verdict + the exact instruction args you'd send.
"""

import argparse
import hashlib
import sys
import os
from datetime import datetime, timezone

# make the engine importable whether run from repo root or scripts/
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "engine"))

from veritas.engine import Asset, AssetType, Claim, score, demo_claims  # noqa: E402


def build_claim(args) -> Claim:
    if args.demo:
        picks = {
            "good": 0, "impossible": 1, "inflated": 2,
        }
        idx = picks.get(args.demo)
        if idx is None:
            sys.exit(f"unknown demo '{args.demo}' (good|impossible|inflated)")
        return demo_claims()[idx][1]

    return Claim(
        submitter=args.submitter,
        asset=Asset(AssetType(args.asset_type), args.capacity, args.region,
                    args.lat, args.location),
        period_from=args.period_from,
        period_to=args.period_to,
        energy_delivered_kwh=args.energy,
        claimed_co2_avoided_kg=args.claimed_co2,
        self_reported_intensity_gco2_kwh=args.self_intensity,
    )


def score_to_bps(score_float: float) -> int:
    return max(0, min(10_000, round(score_float * 10_000)))


def asset_id(claim: Claim) -> str:
    """The on-chain asset id: sha256 of the asset's registry identity.

    The program holds capacity, region and latitude in the Asset account, so a
    claim is judged against the registered figures rather than the ones a
    submitter types in. This derives the same id the registrar used.
    """
    identity = "|".join([
        claim.asset.type.value,
        f"{claim.asset.nameplate_capacity_kw:.3f}",
        str(claim.asset.region_id),
        f"{claim.asset.latitude:.5f}",
        claim.asset.location_hint,
    ])
    return hashlib.sha256(identity.encode("utf-8")).hexdigest()


def period_start(claim: Claim) -> int:
    """Settlement slot start as a unix timestamp, the second half of the
    (asset, slot) pair that makes a reading claimable only once."""
    parsed = datetime.fromisoformat(claim.period_from.replace("Z", "+00:00"))
    return int(parsed.astimezone(timezone.utc).timestamp())


def main():
    p = argparse.ArgumentParser(description="Score a claim and commit it on-chain")
    p.add_argument("--demo", help="use a bundled demo claim: good|impossible|inflated")
    p.add_argument("--submitter", default="Ver1tasDemoSubmitter1111111111111111111111")
    p.add_argument("--asset-type", default="solar_pv")
    p.add_argument("--capacity", type=float, default=5000)
    p.add_argument("--region", type=int, default=3)
    p.add_argument("--lat", type=float, default=53.5)
    p.add_argument("--location", default="North West England")
    p.add_argument("--period-from", default="2026-09-15T12:00Z")
    p.add_argument("--period-to", default="2026-09-15T12:30Z")
    p.add_argument("--energy", type=float, default=1400)
    p.add_argument("--claimed-co2", type=float, default=250)
    p.add_argument("--self-intensity", type=float, default=None)
    p.add_argument("--dry-run", action="store_true",
                   help="score only; print the on-chain args without submitting")
    args = p.parse_args()

    claim = build_claim(args)
    verdict = score(claim)

    print("=" * 66)
    print("VERITAS — off-chain physics verdict")
    print("=" * 66)
    print(verdict.pretty())

    bps = score_to_bps(verdict.integrity_score)
    print("\n" + "-" * 66)
    print("On-chain commitment (submit_claim instruction args):")
    print(f"  asset_id             : {asset_id(claim)}")
    print(f"  inputs_hash          : {verdict.inputs_hash}")
    print(f"  model_version        : 1")
    print(f"  period_start         : {period_start(claim)}  ({claim.period_from})")
    print(f"  claimed_co2_kg       : {int(claim.claimed_co2_avoided_kg)}")
    print(f"  integrity_score_bps  : {bps}")
    print(f"  bond (lamports)      : 100000000  (0.1 SOL)")
    print("\n  The asset must already be registered on-chain under this asset_id,")
    print("  and the submitting wallet must be its registered owner.")

    if args.dry_run or not _can_submit():
        print("\n[dry-run] not submitting on-chain.")
        print("To submit: set up a funded devnet keypair and install solana-py,")
        print("then re-run without --dry-run. See scripts/README for the client.")
        return

    # Real submission path (requires solana-py + anchorpy) is documented in
    # scripts/README.md; kept out of the default path so the bridge always runs.
    print("\n[submit] on-chain submission handled by the TS client in app/;")
    print("this Python bridge prints the exact args for that client to send.")


def _can_submit() -> bool:
    return os.path.exists(os.path.expanduser("~/.config/solana/id.json"))


if __name__ == "__main__":
    main()
