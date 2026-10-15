#!/usr/bin/env python3
"""Bounded local benchmarks. Reports raw samples, costs and unmatched semantics."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
from fuse_integration import Workspace  # noqa: E402


def measured(fn, count):
    samples = []
    for i in range(count):
        start = time.perf_counter_ns()
        fn(i)
        samples.append((time.perf_counter_ns() - start) / 1e6)
    return {"unit": "ms", "samples": samples, "median": statistics.median(samples),
            "min": min(samples), "max": max(samples)}


def run(args, cwd=None):
    return subprocess.run(list(map(str, args)), cwd=cwd, check=True, capture_output=True, timeout=120)


def physical(path):
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()
    report = {"schema": 1, "recorded_at": datetime.now(timezone.utc).isoformat(),
              "platform": platform.platform(), "logical_cpus": os.cpu_count(),
              "environment": "Docker Desktop Linux aarch64 VM on Apple M3 Pro, 18 GB host RAM; VM approximately 8 GB. Single local run, warm host caches.",
              "notes": ["CLI samples include process startup, SQLite open and checkpoint on close.",
                        "Git worktree creation materializes files; WitnessFS snapshot creation does not mount them. Mount startup is measured separately.",
                        "Import is a one-time extra cost; WitnessFS is not an in-place overlay on the source tree.",
                        "Each write sample replaces 64 KiB then fsyncs. Native filesystem is the container writable layer; not a matched storage engine.",
                        "Reads include open/read/close and observe the whole file. Warm kernel caches allowed on both paths.",
                        "Not a throughput, production concurrency, SSD durability or large-repository capacity claim."],
              "scales": []}
    for count in (100, 10000):
        files = {f"src/f{i:05d}.txt": (f"unique source {i}\n".encode() * 128)[:2048] for i in range(count)}
        start = time.perf_counter_ns()
        w = Workspace(files)
        elapsed = (time.perf_counter_ns() - start) / 1e6
        try:
            source = w.root / "source"
            # Measure import itself separately, without fixture generation or Git initialization.
            import_store = w.root / "import-measurement"
            import_ms = measured(lambda _: run(["witnessfs", "--store", import_store, "init", "--from", source]), 1)
            before = w.call("stats")
            disk_before = physical(w.store)
            snapshots = measured(lambda i: w.branch(f"snapshot-{i}"), 25)
            after = w.call("stats")
            disk_after = physical(w.store)
            run(["git", "init", "-b", "main"], source)
            run(["git", "config", "user.name", "Benchmark fixture"], source)
            run(["git", "config", "user.email", "fixture@example.invalid"], source)
            git_import = measured(lambda _: (run(["git", "add", "."], source), run(["git", "commit", "-m", "Fixture"], source)), 1)
            git_samples = measured(lambda i: run(["git", "worktree", "add", "--detach", w.root / f"git-{i}", "HEAD"], source), 8)
            mount_samples = measured(lambda i: mount_once(w, f"snapshot-{i}"), 8)
            accepts = []
            for i in range(10):
                name = f"accept-{i}"
                w.branch(name)
                with w.mount(name) as p:
                    (p / f"result-{i}").write_bytes(b"result")
                w.call("seal", name)
                accepts.extend(measured(lambda _: w.call("accept", name), 1)["samples"])
            report["scales"].append({"files": count, "source_bytes": sum(map(len, files.values())),
                                     "fixture_and_first_import_ms": elapsed, "import": import_ms,
                                     "git_add_and_commit": git_import, "snapshot": snapshots, "git_worktree": git_samples,
                                     "mount_unmount": mount_samples, "accept": {"samples": accepts, "median": statistics.median(accepts), "unit": "ms"},
                                     "storage_before_snapshots": before, "storage_after_snapshots": after,
                                     "disk_bytes_before": disk_before, "disk_bytes_after": disk_after})
        finally:
            w.close()

    payload = bytes(range(256)) * (4 * 1024 * 1024 // 256)
    w = Workspace({"large.bin": payload})
    try:
        w.branch("io")
        native = w.root / "native"
        native.mkdir()
        (native / "large.bin").write_bytes(payload)
        chunk = os.urandom(65536)
        def read(path):
            assert (path / "large.bin").read_bytes() == payload
        def durable_write(path, i):
            with open(path / "output.bin", "wb", buffering=0) as f:
                assert f.write(chunk) == len(chunk)
                os.fsync(f.fileno())
        with w.mount("io") as p:
            # Warm both explicitly, then retain all measured samples.
            read(native); read(p)
            report["io"] = {"read_bytes": len(payload), "write_bytes": len(chunk),
                            "native_read": measured(lambda _: read(native), 25),
                            "witness_read": measured(lambda _: read(p), 25),
                            "native_write_fsync": measured(lambda i: durable_write(native, i), 25),
                            "witness_write_fsync": measured(lambda i: durable_write(p, i), 25)}
        report["io"]["receipt"] = w.accept("io")
    finally:
        w.close()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"scales": [{"files": s["files"], "snapshot_ms": s["snapshot"]["median"], "git_worktree_ms": s["git_worktree"]["median"], "import_ms": s["import"]["median"]} for s in report["scales"]], "io_medians_ms": {k: v["median"] for k, v in report["io"].items() if isinstance(v, dict) and "median" in v}}, indent=2))


def mount_once(w, branch):
    with w.mount(branch):
        pass


if __name__ == "__main__":
    main()
