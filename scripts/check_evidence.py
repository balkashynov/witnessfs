#!/usr/bin/env python3
"""Detect stale source/evidence pairing in a release or copied demo."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
manifest = json.loads((ROOT / "docs/evidence/source-manifest.json").read_text())
for group in ("source", "evidence"):
    for name, expected in manifest[group].items():
        actual = hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit(f"Changed {group} file: {name}. Rerun affected checks and refresh evidence explicitly.")
for name in ("demo", "benchmark", "kernel-tests"):
    assert (ROOT / f"demo/data/{name}.json").read_bytes() == (ROOT / f"docs/evidence/{name}.json").read_bytes(), name
demo = json.loads((ROOT / "demo/data/demo.json").read_text())
assert demo["git"]["merge"]["exit_code"] == 0
assert demo["git"]["combined_tests"][1]["exit_code"] == 1
assert demo["witness"]["stale_receipt"]["outcome"] == "rejected"
assert all(c["exit_code"] == 0 for c in demo["witness"]["final_tests"])
for name, expected in demo["fixture_sha256"].items():
    assert hashlib.sha256((ROOT / "examples/timeout" / name).read_bytes()).hexdigest() == expected
print(f"Verified {len(manifest['source'])} source files, {len(manifest['evidence'])} evidence files and all demo copies.")
