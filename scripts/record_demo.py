#!/usr/bin/env python3
"""Run real Git and mounted WitnessFS histories; save their evidence, not a simulation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
from fuse_integration import Workspace  # noqa: E402

FIXTURE = ROOT / "examples/timeout"
DEFAULT_TEST = "from client import default_timeout; actual=default_timeout(); print(f'default: {actual:g}s (expected 30s)'); assert actual == 30"
RETRY_TEST = "from client import retry_timeout; actual=retry_timeout(); print(f'retry: {actual:g}s (expected 60s)'); assert actual == 60, f'expected 60 seconds, got {actual:g}'"


def command(args, cwd, label, expected=0):
    r = subprocess.run(list(map(str, args)), cwd=cwd, text=True, capture_output=True,
                       env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}, timeout=30)
    assert r.returncode == expected, (args, r.returncode, r.stdout, r.stderr)
    return {"label": label, "command": [str(a).replace(str(ROOT), "$REPO") for a in args],
            "exit_code": r.returncode, "stdout": r.stdout, "stderr": r.stderr}


def task(cwd, name):
    return command(["python3", "-B", FIXTURE / f"task_{name}.py"], cwd, f"{name} task")


def test(cwd, kind, fail=False):
    return command(["python3", "-B", "-c", DEFAULT_TEST if kind == "default" else RETRY_TEST],
                   cwd, f"{kind} test", expected=1 if fail else 0)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    files = {name: (FIXTURE / name).read_bytes() for name in ("client.py", "config.json")}
    report = {"schema": 1, "recorded_at": datetime.now(timezone.utc).isoformat(),
              "platform": platform.platform(), "execution": "Real Linux FUSE and Git; deterministic scripted tasks, no LLM session.",
              "fixture_sha256": {n: hashlib.sha256(b).hexdigest() for n, b in files.items()}}
    with tempfile.TemporaryDirectory(prefix="witness-demo-git-") as temp:
        p = Path(temp)
        for name, data in files.items():
            (p / name).write_bytes(data)
        def git(*a):
            return command(["git", *a], p, "git " + " ".join(a))
        git("init", "-b", "main"); git("config", "user.name", "Demo fixture")
        git("config", "user.email", "fixture@example.invalid")
        git("add", "."); git("commit", "-m", "Base fixture")
        git("branch", "retry"); git("checkout", "-b", "migrate")
        migrate = [task(p, "migrate"), test(p, "default")]
        git("add", "."); git("commit", "-m", "Migrate configuration units")
        git("checkout", "retry")
        retry = [task(p, "retry"), test(p, "retry")]
        git("add", "."); git("commit", "-m", "Add 60 second retry")
        git("checkout", "main"); git("merge", "--ff-only", "migrate")
        merge = git("merge", "--no-edit", "retry")
        report["git"] = {"migrate": migrate, "retry": retry, "merge": merge,
                         "combined_tests": [test(p, "default"), test(p, "retry", fail=True)],
                         "diff": git("show", "--stat", "--oneline", "HEAD")["stdout"]}

    w = Workspace(files)
    try:
        w.branch("migrate"); w.branch("retry")
        with w.mount("migrate") as a, w.mount("retry") as b:
            migrate = [task(a, "migrate"), test(a, "default")]
            retry = [task(b, "retry"), test(b, "retry")]
        reads = w.call("observe", "retry")
        first = w.accept("migrate")
        stale = w.accept("retry", rejected=True)
        assert any(c["kind"] == "content" and c["path"] == "/config.json" for c in stale["conflicts"])
        w.branch("retry-fresh")
        with w.mount("retry-fresh") as p:
            fresh = [task(p, "retry"), test(p, "retry")]
        fixed = w.accept("retry-fresh")
        out = w.root / "final"
        w.call("export", out)
        report["witness"] = {"migrate": migrate, "retry": retry, "retry_observations": reads,
                             "first_receipt": first, "stale_receipt": stale, "fresh": fresh,
                             "fresh_receipt": fixed, "final_tests": [test(out, "default"), test(out, "retry")],
                             "stats": w.call("stats"), "integrity": w.call("check").strip()}
    finally:
        w.close()

    cases = []
    for scenario in ("independent", "missing-path", "directory-phantom", "conservative-rejection"):
        w = Workspace({"left": b"1", "right": b"1", "config": b"value=1\n# old comment\n", "plugins/one": b"one"})
        try:
            w.branch("reader"); w.branch("writer")
            with w.mount("reader") as a, w.mount("writer") as b:
                if scenario == "independent":
                    (a / "left").write_bytes(b"2"); (b / "right").write_bytes(b"3")
                elif scenario == "missing-path":
                    assert not (a / "feature.flag").exists()
                    (a / "decision").write_bytes(b"disabled")
                    (b / "feature.flag").write_bytes(b"enabled")
                elif scenario == "directory-phantom":
                    listing = sorted(os.listdir(a / "plugins"))
                    (a / "catalog").write_text(json.dumps(listing))
                    (b / "plugins/two").write_bytes(b"two")
                else:
                    value = (a / "config").read_text().splitlines()[0]
                    (a / "decision").write_text(value)
                    (b / "config").write_text("value=1\n# new comment\n")
            writer = w.accept("writer")
            reader = w.accept("reader", rejected=scenario != "independent")
            cases.append({"id": scenario, "writer": writer, "reader": reader})
        finally:
            w.close()
    report["cases"] = cases
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"git_merge": "clean", "git_retry_seconds": 0.06,
                      "witness_stale": stale["outcome"], "fresh": fixed["outcome"],
                      "other_cases": len(cases), "output": str(args.output)}, indent=2))


if __name__ == "__main__":
    main()
