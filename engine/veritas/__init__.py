"""Veritas — physics-informed plausibility engine for clean-energy carbon claims."""
from .engine import (
    Asset, AssetType, Claim, Verdict, CheckResult, Status,
    score, fetch_grid_intensity, demo_claims,
)

__version__ = "0.1.0"
__all__ = [
    "Asset", "AssetType", "Claim", "Verdict", "CheckResult", "Status",
    "score", "fetch_grid_intensity", "demo_claims", "__version__",
]
