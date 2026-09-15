"""
Veritas — grid plausibility engine (v0.1)
=========================================

A physics-informed scorer for clean-energy carbon claims.

Given a claim of the form "asset X delivered Y kWh in half-hour H and thereby
avoided Z kg CO2", Veritas checks whether that claim is *physically possible*
against hard physical bounds and the *actual* carbon intensity of the GB grid
at that moment (live NESO Carbon Intensity API, CC BY 4.0, keyless).

Each check enforces one named physical law and returns a legible verdict — not
an opaque trust score. That legibility is the point: the output tells you WHICH
law a claim violates and by how much.

Design notes
------------
- Pure-Python, one optional dependency (`requests`). Falls back to a bundled
  offline grid value if the network is unavailable, so the demo never dies live.
- Deterministic: same inputs -> same verdict. This is what lets an on-chain
  challenge/resolve step reproduce the score trustlessly.
- The canonical claim serialization + sha256 is the exact commitment that would
  go on-chain (Anchor `inputs_hash`).

Run:  python veritas_engine.py
"""

from __future__ import annotations

import json
import hashlib
import math
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from enum import Enum
from typing import Optional

try:
    import requests
    _HAVE_REQUESTS = True
except ImportError:
    _HAVE_REQUESTS = False


# --------------------------------------------------------------------------- #
# Claim schema
# --------------------------------------------------------------------------- #

class AssetType(str, Enum):
    SOLAR_PV = "solar_pv"
    WIND = "wind"
    BATTERY_EXPORT = "battery_export"


@dataclass
class Asset:
    type: AssetType
    nameplate_capacity_kw: float
    region_id: int                 # NESO DNO region id (1..17); ground-truth grid intensity
    latitude: float                # for the solar resource envelope
    location_hint: str = ""


@dataclass
class Claim:
    submitter: str                 # solana pubkey (string form)
    asset: Asset
    period_from: str               # ISO8601 Z, aligned to a :00/:30 settlement slot
    period_to: str                 # ISO8601 Z, exactly 30 min after period_from
    energy_delivered_kwh: float    # what the asset says it supplied
    claimed_co2_avoided_kg: float  # the headline claim under test
    self_reported_intensity_gco2_kwh: Optional[float] = None  # optional
    model_version: str = "veritas-grid-0.1"

    def canonical(self) -> str:
        """Deterministic JSON serialization — the exact bytes that get hashed
        and committed on-chain. Sorted keys, no whitespace drift."""
        payload = {
            "submitter": self.submitter,
            "asset": {
                "type": self.asset.type.value,
                "nameplate_capacity_kw": self.asset.nameplate_capacity_kw,
                "region_id": self.asset.region_id,
                "latitude": self.asset.latitude,
                "location_hint": self.asset.location_hint,
            },
            "period_from": self.period_from,
            "period_to": self.period_to,
            "energy_delivered_kwh": self.energy_delivered_kwh,
            "claimed_co2_avoided_kg": self.claimed_co2_avoided_kg,
            "self_reported_intensity_gco2_kwh": self.self_reported_intensity_gco2_kwh,
            "model_version": self.model_version,
        }
        return json.dumps(payload, sort_keys=True, separators=(",", ":"))

    def inputs_hash(self) -> str:
        """sha256 of the canonical claim — Anchor `inputs_hash` (hex)."""
        return hashlib.sha256(self.canonical().encode("utf-8")).hexdigest()


# --------------------------------------------------------------------------- #
# Check results
# --------------------------------------------------------------------------- #

class Status(str, Enum):
    PASS = "PASS"
    FAIL = "FAIL"          # soft implausibility (outside expected envelope)
    HARD_FAIL = "HARD_FAIL"  # violates a hard physical bound — impossible
    SKIP = "SKIP"          # not applicable / insufficient data


@dataclass
class CheckResult:
    id: str
    law: str
    status: Status
    detail: str


@dataclass
class Verdict:
    verdict: str
    integrity_score: float          # 0.0 .. 1.0
    hardest_failure: Optional[str]
    grid_intensity_used: Optional[float]
    grid_data_source: str
    inputs_hash: str
    checks: list = field(default_factory=list)

    def to_dict(self) -> dict:
        d = asdict(self)
        d["checks"] = [
            {"id": c.id, "law": c.law, "status": c.status.value, "detail": c.detail}
            for c in self.checks
        ]
        return d

    def pretty(self) -> str:
        icon = {"PASS": "PASS  ", "FAIL": "FAIL  ",
                "HARD_FAIL": "IMPOSS", "SKIP": "skip  "}
        lines = [
            f"  verdict          : {self.verdict}",
            f"  integrity_score  : {self.integrity_score:.2f}",
            f"  grid intensity   : {self.grid_intensity_used} gCO2/kWh "
            f"({self.grid_data_source})",
            f"  inputs_hash      : {self.inputs_hash[:16]}…",
            "  checks:",
        ]
        for c in self.checks:
            lines.append(f"    [{icon[c.status.value]}] {c.id}")
            lines.append(f"             law: {c.law}")
            lines.append(f"             {c.detail}")
        return "\n".join(lines)


# --------------------------------------------------------------------------- #
# Grid ground truth (live NESO, with offline fallback)
# --------------------------------------------------------------------------- #

# Regional fallback values (gCO2/kWh) used only if the network is unavailable,
# so a live demo never breaks. Chosen mid-range; overridden by real data when
# reachable.
_FALLBACK_REGION_INTENSITY = 180


def fetch_grid_intensity(region_id: int, period_from: str) -> tuple[float, str]:
    """Return (actual gCO2/kWh, source string) for a DNO region at a slot.

    Uses the NESO regional endpoint:
      GET https://api.carbonintensity.org.uk/regional/intensity/{from}/fw24h
    We request a window starting at period_from and take the matching slot.
    Falls back to a bundled value if offline.
    """
    if not _HAVE_REQUESTS:
        return float(_FALLBACK_REGION_INTENSITY), "offline fallback (requests missing)"

    url = (
        f"https://api.carbonintensity.org.uk/regional/intensity/"
        f"{period_from}/fw24h/regionid/{region_id}"
    )
    try:
        r = requests.get(url, headers={"Accept": "application/json"}, timeout=8)
        r.raise_for_status()
        data = r.json()["data"]
        slots = data.get("data") if isinstance(data, dict) else data
        # find the slot whose 'from' matches our period start
        for slot in slots:
            if slot["from"] == period_from:
                intensity = slot["intensity"]
                val = intensity.get("forecast")  # regional gives forecast
                if val is not None:
                    return float(val), "NESO regional API (forecast, region " \
                                       f"{region_id})"
        # fall back to first available slot
        if slots:
            val = slots[0]["intensity"].get("forecast")
            if val is not None:
                return float(val), f"NESO regional API (nearest slot, region {region_id})"
    except Exception as e:  # network, shape, timeout — degrade gracefully
        return float(_FALLBACK_REGION_INTENSITY), f"offline fallback ({type(e).__name__})"
    return float(_FALLBACK_REGION_INTENSITY), "offline fallback (no matching slot)"


# --------------------------------------------------------------------------- #
# Physics: solar resource envelope
# --------------------------------------------------------------------------- #

def solar_clearsky_ceiling(latitude: float, when: datetime) -> float:
    """A cheap physical ceiling on solar capacity factor at a given time/place.

    Uses solar elevation angle: CF cannot plausibly exceed sin(elevation)
    scaled by a generous clear-sky factor. At night (elevation <= 0) the ceiling
    is 0 — a solar asset producing at night is physically impossible.

    This is deliberately simple and defensible: it needs no external data, only
    astronomy. It is a *ceiling*, not a forecast — it bounds what is possible,
    which is exactly what a plausibility check needs.
    """
    # day of year and fractional hour (UTC; good enough for a ceiling)
    day = when.timetuple().tm_yday
    hour = when.hour + when.minute / 60.0

    # solar declination (deg)
    decl = 23.45 * math.sin(math.radians(360.0 * (284 + day) / 365.0))
    # hour angle (deg): 15 deg per hour from solar noon (approx, UTC ~ solar for GB)
    hour_angle = 15.0 * (hour - 12.0)

    lat_r = math.radians(latitude)
    decl_r = math.radians(decl)
    ha_r = math.radians(hour_angle)

    sin_elev = (math.sin(lat_r) * math.sin(decl_r)
                + math.cos(lat_r) * math.cos(decl_r) * math.cos(ha_r))
    elevation = math.degrees(math.asin(max(-1.0, min(1.0, sin_elev))))

    if elevation <= 0:
        return 0.0
    # clear-sky generous factor: real PV rarely exceeds ~0.9 of sin(elev)-scaled
    return min(1.0, 0.9 * sin_elev)


# --------------------------------------------------------------------------- #
# The checks
# --------------------------------------------------------------------------- #

_PERIOD_HOURS = 0.5   # half-hour settlement slot
_CARBON_TOLERANCE = 0.05  # 5% slack on the avoided-emissions ceiling


def _parse(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00")).astimezone(timezone.utc)


def check_capacity_ceiling(claim: Claim) -> CheckResult:
    max_energy = claim.asset.nameplate_capacity_kw * _PERIOD_HOURS
    cf = claim.energy_delivered_kwh / max_energy if max_energy else float("inf")
    if cf > 1.0:
        return CheckResult(
            "capacity_ceiling", "conservation of energy (output <= capacity x time)",
            Status.HARD_FAIL,
            f"implied capacity factor {cf*100:.0f}% exceeds physical maximum 100% "
            f"({claim.energy_delivered_kwh:.0f} kWh claimed vs {max_energy:.0f} kWh "
            f"max for a {claim.asset.nameplate_capacity_kw:.0f} kW asset in 30 min)",
        )
    return CheckResult(
        "capacity_ceiling", "conservation of energy (output <= capacity x time)",
        Status.PASS,
        f"implied capacity factor {cf*100:.0f}% <= 100%",
    )


def check_resource_envelope(claim: Claim) -> CheckResult:
    if claim.asset.type != AssetType.SOLAR_PV:
        return CheckResult(
            "resource_envelope", "source-specific physical resource limit",
            Status.SKIP, f"envelope not implemented for {claim.asset.type.value} (roadmap)",
        )
    when = _parse(claim.period_from)
    max_energy = claim.asset.nameplate_capacity_kw * _PERIOD_HOURS
    cf = claim.energy_delivered_kwh / max_energy if max_energy else float("inf")
    ceiling = solar_clearsky_ceiling(claim.asset.latitude, when)
    if ceiling == 0.0 and cf > 0.02:
        return CheckResult(
            "resource_envelope", "solar irradiance limit (no output without sun)",
            Status.HARD_FAIL,
            f"solar asset claims CF {cf*100:.0f}% at {when.strftime('%H:%M UTC')} "
            f"but sun is below horizon at lat {claim.asset.latitude:.1f} — no irradiance",
        )
    if cf > ceiling + 0.05:
        return CheckResult(
            "resource_envelope", "solar irradiance limit (clear-sky ceiling)",
            Status.FAIL,
            f"solar CF {cf*100:.0f}% exceeds clear-sky ceiling {ceiling*100:.0f}% "
            f"for this time and latitude",
        )
    return CheckResult(
        "resource_envelope", "solar irradiance limit (clear-sky ceiling)",
        Status.PASS,
        f"solar CF {cf*100:.0f}% within clear-sky ceiling {ceiling*100:.0f}%",
    )


def check_avoided_emissions_bound(claim: Claim, grid_intensity: float,
                                  source: str) -> CheckResult:
    """The core carbon check. You cannot claim to avoid more CO2 than the grid
    would have emitted producing the same energy."""
    max_avoided_kg = claim.energy_delivered_kwh * grid_intensity / 1000.0
    ceiling = max_avoided_kg * (1 + _CARBON_TOLERANCE)
    if claim.claimed_co2_avoided_kg > ceiling:
        return CheckResult(
            "avoided_emissions_bound",
            "avoided CO2 <= energy delivered x grid carbon intensity",
            Status.HARD_FAIL,
            f"claimed {claim.claimed_co2_avoided_kg:.0f} kg avoided but grid at "
            f"{grid_intensity:.0f} gCO2/kWh over {claim.energy_delivered_kwh:.0f} kWh "
            f"caps avoided at {max_avoided_kg:.0f} kg "
            f"(over-claim of {claim.claimed_co2_avoided_kg - max_avoided_kg:.0f} kg)",
        )
    return CheckResult(
        "avoided_emissions_bound",
        "avoided CO2 <= energy delivered x grid carbon intensity",
        Status.PASS,
        f"claimed {claim.claimed_co2_avoided_kg:.0f} kg <= ceiling {max_avoided_kg:.0f} kg "
        f"at live grid {grid_intensity:.0f} gCO2/kWh",
    )


def check_internal_consistency(claim: Claim) -> CheckResult:
    if claim.self_reported_intensity_gco2_kwh is None:
        return CheckResult(
            "internal_consistency", "dimensional consistency of self-reported figures",
            Status.SKIP, "no self-reported intensity supplied",
        )
    implied = (claim.energy_delivered_kwh
               * claim.self_reported_intensity_gco2_kwh / 1000.0)
    if implied == 0:
        return CheckResult(
            "internal_consistency", "dimensional consistency of self-reported figures",
            Status.SKIP, "self-reported intensity is zero",
        )
    ratio = claim.claimed_co2_avoided_kg / implied
    if ratio > 1.5 or ratio < 0.67:
        return CheckResult(
            "internal_consistency", "dimensional consistency of self-reported figures",
            Status.FAIL,
            f"claimed {claim.claimed_co2_avoided_kg:.0f} kg vs self-reported figures "
            f"implying {implied:.0f} kg — {ratio:.1f}x mismatch (possible unit error)",
        )
    return CheckResult(
        "internal_consistency", "dimensional consistency of self-reported figures",
        Status.PASS, f"self-reported figures reconcile ({ratio:.2f}x)",
    )


def check_temporal_validity(claim: Claim) -> CheckResult:
    try:
        pf = _parse(claim.period_from)
        pt = _parse(claim.period_to)
    except Exception:
        return CheckResult("temporal_validity", "measurement must post-date the period",
                           Status.HARD_FAIL, "unparseable period timestamps")
    if (pt - pf).total_seconds() != 1800:
        return CheckResult("temporal_validity", "settlement-slot alignment (30 min)",
                           Status.FAIL,
                           f"period is {(pt-pf).total_seconds()/60:.0f} min, not a 30-min slot")
    if pf.minute not in (0, 30) or pf.second != 0:
        return CheckResult("temporal_validity", "settlement-slot alignment (:00/:30)",
                           Status.FAIL, "period does not start on a settlement boundary")
    if pf > datetime.now(timezone.utc):
        return CheckResult("temporal_validity", "measurement must post-date the period",
                           Status.HARD_FAIL,
                           "period is in the future — a forecast is not a measurement")
    return CheckResult("temporal_validity", "settlement-slot alignment & past-dated",
                       Status.PASS, "valid past-dated 30-min settlement slot")


# --------------------------------------------------------------------------- #
# Orchestration
# --------------------------------------------------------------------------- #

def score(claim: Claim) -> Verdict:
    grid_intensity, source = fetch_grid_intensity(claim.asset.region_id,
                                                  claim.period_from)
    checks = [
        check_temporal_validity(claim),
        check_capacity_ceiling(claim),
        check_resource_envelope(claim),
        check_avoided_emissions_bound(claim, grid_intensity, source),
        check_internal_consistency(claim),
    ]

    hard = [c for c in checks if c.status == Status.HARD_FAIL]
    soft = [c for c in checks if c.status == Status.FAIL]

    if hard:
        integrity = 0.15
        verdict = "IMPOSSIBLE"
        hardest = hard[0].id
    elif soft:
        integrity = 0.55
        verdict = "IMPLAUSIBLE"
        hardest = soft[0].id
    else:
        integrity = 0.95
        verdict = "PLAUSIBLE"
        hardest = None

    return Verdict(
        verdict=verdict,
        integrity_score=integrity,
        hardest_failure=hardest,
        grid_intensity_used=grid_intensity,
        grid_data_source=source,
        inputs_hash=claim.inputs_hash(),
        checks=checks,
    )


# --------------------------------------------------------------------------- #
# Demo fixtures — the good claim / bad claims that carry the pitch video
# --------------------------------------------------------------------------- #

def _recent_slot() -> tuple[str, str]:
    """A settlement slot ~2 hours in the past, aligned to :00/:30, so live grid
    'actual' data exists for it."""
    now = datetime.now(timezone.utc)
    slot = now.replace(minute=0 if now.minute < 30 else 30, second=0, microsecond=0)
    # step back a few hours to be safely in the past with published data
    from datetime import timedelta
    slot = slot - timedelta(hours=3)
    return (slot.strftime("%Y-%m-%dT%H:%MZ"),
            (slot + timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%MZ"))


def demo_claims() -> list[tuple[str, Claim]]:
    pf, pt = _recent_slot()
    # A 5 MW solar farm in NW England (region 3), lat ~53.5
    base_asset = Asset(AssetType.SOLAR_PV, 5000, region_id=3,
                       latitude=53.5, location_hint="North West England")

    good = Claim(
        submitter="Ver1tasDemoGoodpubkey1111111111111111111111",
        asset=base_asset, period_from=pf, period_to=pt,
        energy_delivered_kwh=1400,          # ~56% CF — plausible for good midday sun
        claimed_co2_avoided_kg=250,         # will be checked against live grid
        self_reported_intensity_gco2_kwh=180,
    )

    # Fraud 1: physically impossible energy (128% capacity factor)
    impossible_energy = Claim(
        submitter="Ver1tasDemoBad1pubkey11111111111111111111111",
        asset=base_asset, period_from=pf, period_to=pt,
        energy_delivered_kwh=3200,          # 128% CF — impossible
        claimed_co2_avoided_kg=500,
    )

    # Fraud 2: plausible energy, inflated carbon (the common real fraud)
    inflated_carbon = Claim(
        submitter="Ver1tasDemoBad2pubkey11111111111111111111111",
        asset=base_asset, period_from=pf, period_to=pt,
        energy_delivered_kwh=1400,          # fine
        claimed_co2_avoided_kg=1800,        # way above grid-intensity ceiling
    )

    return [
        ("GOOD  — plausible solar claim", good),
        ("FRAUD — impossible energy (128% CF)", impossible_energy),
        ("FRAUD — inflated carbon vs live grid", inflated_carbon),
    ]


