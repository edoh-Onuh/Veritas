from .engine import demo_claims, score
import json

if __name__ == "__main__":
    print("=" * 70)
    print("VERITAS grid plausibility engine")
    print("=" * 70)
    v = None
    for label, claim in demo_claims():
        print(f"\n### {label}")
        v = score(claim)
        print(v.pretty())
    if v:
        print("\n" + "=" * 70)
        print("JSON output of last verdict (committed on-chain):")
        print(json.dumps(v.to_dict(), indent=2))
