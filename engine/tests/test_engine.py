"""
Deterministic tests for the Veritas plausibility engine.

These run fully offline: grid intensity is injected via monkeypatch so the
suite never depends on the live NESO API. Every physics check has a pass case
and a fail case, and the two headline fraud types are asserted end-to-end.
"""

import pytest
from veritas.engine import (
    Asset, AssetType, Claim, Status, score, fetch_grid_intensity,
    check_capacity_ceiling, check_avoided_emissions_bound,
    check_resource_envelope, check_internal_consistency,
    check_temporal_validity, solar_clearsky_ceiling,
)
from datetime import datetime, timezone, timedelta


# --- helpers --------------------------------------------------------------- #

def _past_slot(hours_back=3):
    now = datetime.now(timezone.utc)
    slot = now.replace(minute=0 if now.minute < 30 else 30,
                       second=0, microsecond=0) - timedelta(hours=hours_back)
    return (slot.strftime("%Y-%m-%dT%H:%MZ"),
            (slot + timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%MZ"))


def _noon_slot():
    """A slot around solar noon so the solar envelope permits real output."""
    now = datetime.now(timezone.utc)
    slot = now.replace(hour=12, minute=0, second=0, microsecond=0) - timedelta(days=1)
    return (slot.strftime("%Y-%m-%dT%H:%MZ"),
            (slot + timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%MZ"))


def _asset(cap=5000, region=3, lat=53.5):
    return Asset(AssetType.SOLAR_PV, cap, region, lat, "North West England")


@pytest.fixture(autouse=True)
def stub_grid(monkeypatch):
    """Force a known grid intensity so tests are deterministic and offline."""
    monkeypatch.setattr(
        "veritas.engine.fetch_grid_intensity",
        lambda region_id, period_from: (180.0, "test stub"),
    )


# --- capacity ceiling (conservation of energy) ----------------------------- #

def test_capacity_ceiling_pass():
    pf, pt = _noon_slot()
    c = Claim("x", _asset(), pf, pt, energy_delivered_kwh=1400,
              claimed_co2_avoided_kg=250)
    assert check_capacity_ceiling(c).status == Status.PASS


def test_capacity_ceiling_hard_fail_over_100pct():
    pf, pt = _noon_slot()
    # 3200 kWh from a 5 MW asset in 30 min => 128% CF, impossible
    c = Claim("x", _asset(), pf, pt, energy_delivered_kwh=3200,
              claimed_co2_avoided_kg=500)
    r = check_capacity_ceiling(c)
    assert r.status == Status.HARD_FAIL
    assert "128%" in r.detail


# --- avoided-emissions bound (the core carbon check) ----------------------- #

def test_avoided_emissions_pass_within_ceiling():
    c = Claim("x", _asset(), "2026-01-01T12:00Z", "2026-01-01T12:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=250)
    # ceiling = 1400 * 180 / 1000 = 252 kg; 250 <= 252
    r = check_avoided_emissions_bound(c, 180.0, "test")
    assert r.status == Status.PASS


def test_avoided_emissions_hard_fail_inflated_carbon():
    c = Claim("x", _asset(), "2026-01-01T12:00Z", "2026-01-01T12:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=1800)
    r = check_avoided_emissions_bound(c, 180.0, "test")
    assert r.status == Status.HARD_FAIL
    assert "over-claim" in r.detail


def test_avoided_ceiling_tightens_on_clean_grid():
    """Same claim; cleaner grid => tighter ceiling => catches what a dirty grid
    would have let pass. This is the live-data edge."""
    c = Claim("x", _asset(), "2026-01-01T12:00Z", "2026-01-01T12:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=250)
    assert check_avoided_emissions_bound(c, 180.0, "test").status == Status.PASS
    assert check_avoided_emissions_bound(c, 42.0, "test").status == Status.HARD_FAIL


# --- solar resource envelope ---------------------------------------------- #

def test_solar_envelope_zero_at_night():
    midnight = datetime(2026, 6, 21, 0, 15, tzinfo=timezone.utc)
    assert solar_clearsky_ceiling(53.5, midnight) == 0.0


def test_solar_envelope_positive_at_noon():
    noon = datetime(2026, 6, 21, 12, 0, tzinfo=timezone.utc)
    assert solar_clearsky_ceiling(53.5, noon) > 0.3


def test_resource_envelope_hard_fail_solar_at_night():
    # midnight slot, solar claiming meaningful output
    c = Claim("x", _asset(), "2026-06-21T00:00Z", "2026-06-21T00:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=250)
    r = check_resource_envelope(c)
    assert r.status == Status.HARD_FAIL
    assert "horizon" in r.detail or "irradiance" in r.detail


# --- internal consistency -------------------------------------------------- #

def test_internal_consistency_pass():
    c = Claim("x", _asset(), "2026-01-01T12:00Z", "2026-01-01T12:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=252,
              self_reported_intensity_gco2_kwh=180)
    assert check_internal_consistency(c).status == Status.PASS


def test_internal_consistency_unit_error():
    # claims kg but figures imply a 1000x tonne/kg mixup
    c = Claim("x", _asset(), "2026-01-01T12:00Z", "2026-01-01T12:30Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=252000,
              self_reported_intensity_gco2_kwh=180)
    assert check_internal_consistency(c).status == Status.FAIL


# --- temporal validity ----------------------------------------------------- #

def test_temporal_validity_future_rejected():
    future = datetime.now(timezone.utc) + timedelta(days=2)
    pf = future.replace(minute=0, second=0, microsecond=0)
    c = Claim("x", _asset(), pf.strftime("%Y-%m-%dT%H:%MZ"),
              (pf + timedelta(minutes=30)).strftime("%Y-%m-%dT%H:%MZ"),
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=250)
    assert check_temporal_validity(c).status == Status.HARD_FAIL


def test_temporal_validity_misaligned_slot():
    c = Claim("x", _asset(), "2026-01-01T12:07Z", "2026-01-01T12:37Z",
              energy_delivered_kwh=1400, claimed_co2_avoided_kg=250)
    assert check_temporal_validity(c).status == Status.FAIL


# --- end-to-end verdicts --------------------------------------------------- #

def test_e2e_good_claim_plausible():
    pf, pt = _noon_slot()
    c = Claim("good", _asset(), pf, pt, energy_delivered_kwh=1400,
              claimed_co2_avoided_kg=250, self_reported_intensity_gco2_kwh=180)
    v = score(c)
    assert v.verdict == "PLAUSIBLE"
    assert v.integrity_score > 0.9


def test_e2e_impossible_energy_caught():
    pf, pt = _noon_slot()
    c = Claim("bad1", _asset(), pf, pt, energy_delivered_kwh=3200,
              claimed_co2_avoided_kg=500)
    v = score(c)
    assert v.verdict == "IMPOSSIBLE"
    assert v.hardest_failure == "capacity_ceiling"


def test_e2e_inflated_carbon_caught():
    pf, pt = _noon_slot()
    c = Claim("bad2", _asset(), pf, pt, energy_delivered_kwh=1400,
              claimed_co2_avoided_kg=1800)
    v = score(c)
    assert v.verdict == "IMPOSSIBLE"
    assert v.hardest_failure == "avoided_emissions_bound"


def test_inputs_hash_is_deterministic():
    pf, pt = _noon_slot()
    c1 = Claim("h", _asset(), pf, pt, 1400, 250)
    c2 = Claim("h", _asset(), pf, pt, 1400, 250)
    assert c1.inputs_hash() == c2.inputs_hash()
    c3 = Claim("h", _asset(), pf, pt, 1401, 250)  # one kWh different
    assert c1.inputs_hash() != c3.inputs_hash()
