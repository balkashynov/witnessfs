#!/usr/bin/env python3
"""Real kernel integration tests. Run inside the provided Linux image."""
import contextlib
import ctypes
import errno
import json
import mmap
import os
from pathlib import Path
import platform
import random
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


class Workspace:
    def __init__(self, files=None):
        self.tmp = tempfile.TemporaryDirectory(prefix="witness-fuse-")
        self.root = Path(self.tmp.name)
        source = self.root / "source"
        source.mkdir()
        for name, data in (files or {}).items():
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.store = self.root / "store"
        self.call("init", "--from", source)

    def call(self, *args, expected=0):
        result = subprocess.run(
            ["witnessfs", "--store", str(self.store), *map(str, args)],
            capture_output=True, text=True, timeout=30,
        )
        if result.returncode != expected:
            raise AssertionError(f"{args}: exit {result.returncode}, expected {expected}\n{result.stderr}\n{result.stdout}")
        return json.loads(result.stdout) if result.stdout.strip().startswith(("{", "[")) else result.stdout

    def branch(self, name):
        return self.call("branch", name)

    def accept(self, name, rejected=False):
        self.call("seal", name)
        return self.call("accept", name, expected=3 if rejected else 0)

    @contextlib.contextmanager
    def mount(self, branch):
        mountpoint = self.root / f"mount-{branch}"
        mountpoint.mkdir(exist_ok=True)
        log = open(self.root / f"mount-{branch}.log", "w+")
        process = subprocess.Popen(["witnessfs", "--store", str(self.store), "mount", branch, str(mountpoint)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while not os.path.ismount(mountpoint):
                if process.poll() is not None or time.monotonic() > deadline:
                    log.seek(0)
                    raise AssertionError(f"mount failed: {log.read()}")
                time.sleep(0.01)
            yield mountpoint
        finally:
            if os.path.ismount(mountpoint):
                subprocess.run(["fusermount3", "-u", str(mountpoint)], check=True, timeout=10)
            if process.poll() is None:
                process.wait(timeout=10)
            log.close()

    def close(self):
        self.tmp.cleanup()


class KernelTests(unittest.TestCase):
    def workspace(self, files=None):
        w = Workspace(files)
        self.addCleanup(w.close)
        return w

    def test_python_and_bash_roundtrip(self):
        w = self.workspace({"input.txt": b"observed\n"})
        w.branch("tools")
        with w.mount("tools") as p:
            subprocess.run(["bash", "-c", "cat input.txt | tr a-z A-Z > output.txt"], cwd=p, check=True)
            self.assertEqual((p / "output.txt").read_bytes(), b"OBSERVED\n")
        w.accept("tools")
        out = w.root / "export"
        w.call("export", out)
        self.assertEqual((out / "output.txt").read_bytes(), b"OBSERVED\n")

    def test_clean_writes_allow_both_branches(self):
        w = self.workspace({"a": b"1", "b": b"1"})
        for name in ("one", "two"):
            w.branch(name)
        with w.mount("one") as a, w.mount("two") as b:
            (a / "a").write_bytes(b"2")
            (b / "b").write_bytes(b"3")
        self.assertEqual(w.accept("one")["outcome"], "accepted")
        self.assertEqual(w.accept("two")["outcome"], "accepted")

    def test_stale_read_rejects_disjoint_output(self):
        w = self.workspace({"units": b"seconds", "worker": b"timeout=30"})
        w.branch("config"); w.branch("consumer")
        with w.mount("config") as a, w.mount("consumer") as b:
            self.assertEqual((b / "units").read_bytes(), b"seconds")
            (b / "worker").write_bytes(b"timeout=60")
            (a / "units").write_bytes(b"milliseconds")
        w.accept("config")
        receipt = w.accept("consumer", rejected=True)
        self.assertTrue(any(c["kind"] == "content" and c["path"].endswith("units") for c in receipt["conflicts"]))

    def test_missing_file_dependency(self):
        w = self.workspace()
        w.branch("reader"); w.branch("creator")
        with w.mount("reader") as p:
            self.assertFalse((p / "feature.flag").exists())
            (p / "decision").write_bytes(b"disabled")
        with w.mount("creator") as p:
            (p / "feature.flag").write_bytes(b"enabled")
        w.accept("creator")
        self.assertEqual(w.accept("reader", rejected=True)["outcome"], "rejected")

    def test_directory_phantom(self):
        w = self.workspace({"plugins/one": b"1"})
        w.branch("catalog"); w.branch("plugin")
        with w.mount("catalog") as p:
            self.assertEqual(os.listdir(p / "plugins"), ["one"])
            (p / "catalog").write_bytes(b"one")
        with w.mount("plugin") as p:
            (p / "plugins/two").write_bytes(b"2")
        w.accept("plugin")
        self.assertTrue(any(c["kind"] == "directory" for c in w.accept("catalog", rejected=True)["conflicts"]))

    def test_cached_mmap_read_is_observed(self):
        w = self.workspace({"file": b"original"})
        w.branch("mapped"); w.branch("writer")
        with w.mount("mapped") as p:
            with open(p / "file", "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
                self.assertEqual(m[:], b"original")
            with open(p / "file", "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
                self.assertEqual(m[:], b"original")
            (p / "result").write_bytes(b"used mmap")
        with w.mount("writer") as p:
            (p / "file").write_bytes(b"modified")
        w.accept("writer")
        w.accept("mapped", rejected=True)

    def test_mmap_write_flush_persists(self):
        w = self.workspace({"file": b"original"})
        w.branch("mapped")
        with w.mount("mapped") as p:
            with open(p / "file", "r+b") as f, mmap.mmap(f.fileno(), 0) as m:
                m[:4] = b"EDIT"
                m.flush()
            self.assertEqual((p / "file").read_bytes(), b"EDITinal")
        w.accept("mapped")
        out = w.root / "export"
        w.call("export", out)
        self.assertEqual((out / "file").read_bytes(), b"EDITinal")

    def test_retained_cache_is_coherent_after_writes_and_truncation(self):
        w = self.workspace({"file": b"a" * 8192})
        w.branch("cached")
        with w.mount("cached") as p:
            self.assertEqual((p / "file").read_bytes(), b"a" * 8192)
            os.link(p / "file", p / "alias")
            with open(p / "alias", "r+b", buffering=0) as f:
                f.seek(4096); f.write(b"z" * 4096); os.fsync(f.fileno())
            self.assertEqual((p / "file").read_bytes(), b"a" * 4096 + b"z" * 4096)
            with open(p / "file", "r+b") as f, mmap.mmap(f.fileno(), 0) as m:
                m[0:4] = b"EDIT"; m.flush()
            self.assertEqual((p / "alias").read_bytes()[:4], b"EDIT")
            os.truncate(p / "alias", 3)
            self.assertEqual((p / "file").read_bytes(), b"EDI")
            os.truncate(p / "file", 8192)
            self.assertEqual((p / "alias").read_bytes(), b"EDI" + b"\0" * 8189)
        # A new mount must reconstruct the same bytes without the old cache.
        with w.mount("cached") as p:
            self.assertEqual((p / "file").read_bytes(), b"EDI" + b"\0" * 8189)
        w.accept("cached")

    def test_open_unlink_and_hardlink(self):
        w = self.workspace({"file": b"hello"})
        w.branch("io")
        with w.mount("io") as p:
            os.link(p / "file", p / "alias")
            self.assertEqual(os.stat(p / "file").st_ino, os.stat(p / "alias").st_ino)
            with open(p / "file", "r+b") as f:
                os.unlink(p / "file")
                f.write(b"HELLO"); f.flush(); os.fsync(f.fileno())
                f.seek(0); self.assertEqual(f.read(), b"HELLO")
            self.assertEqual((p / "alias").read_bytes(), b"HELLO")
            self.assertEqual(os.stat(p / "alias").st_nlink, 1)
        w.accept("io")

    def test_rename_replace_and_directory_move(self):
        w = self.workspace({"a": b"a", "b": b"b", "dir/child": b"child"})
        w.branch("io")
        with w.mount("io") as p:
            os.replace(p / "a", p / "b")
            self.assertEqual((p / "b").read_bytes(), b"a")
            os.rename(p / "dir", p / "moved")
            self.assertEqual((p / "moved/child").read_bytes(), b"child")
            with self.assertRaises(OSError):
                os.rename(p / "moved", p / "moved/nested")
        w.accept("io")

    def test_non_utf8_names_and_symlink(self):
        w = self.workspace()
        w.branch("io")
        with w.mount("io") as p:
            name = os.fsencode(p) + b"/raw-\xff"
            with open(name, "wb") as f:
                f.write(b"bytes")
            os.symlink(b"raw-\xff", os.fsencode(p) + b"/alias")
            self.assertEqual((p / "alias").read_bytes(), b"bytes")
        w.accept("io")

    def test_rename_noreplace_checks_even_same_inode(self):
        w = self.workspace({"file": b"value"})
        w.branch("rename")
        libc = ctypes.CDLL(None, use_errno=True)
        rename = libc.renameat2
        rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        with w.mount("rename") as p:
            os.link(p / "file", p / "alias")
            for destination in ("file", "alias"):
                result = rename(-100, os.fsencode(p / "file"), -100, os.fsencode(p / destination), 1)
                self.assertEqual(result, -1)
                self.assertEqual(ctypes.get_errno(), errno.EEXIST)
            self.assertEqual(rename(-100, os.fsencode(p / "file"), -100, os.fsencode(p / "new"), 1), 0)
            self.assertEqual((p / "new").read_bytes(), b"value")
        w.accept("rename")

    def test_compiled_executable_runs_from_mount(self):
        w = self.workspace({"hello.c": b'#include <stdio.h>\nint main(void){puts("real executable");return 0;}\n'})
        w.branch("build")
        with w.mount("build") as p:
            subprocess.run(["gcc", "hello.c", "-o", "hello"], cwd=p, check=True, capture_output=True)
            result = subprocess.run([str(p / "hello")], cwd=p, check=True, capture_output=True)
            self.assertEqual(result.stdout, b"real executable\n")
        w.accept("build")

    def test_out_of_band_mutation_and_seal_are_blocked(self):
        w = self.workspace({"file": b"content"})
        w.branch("live")
        with w.mount("live"):
            w.call("seal", "live", expected=1)
            w.call("read", "live", "file", expected=1)
        w.accept("live")

    def test_sparse_truncate(self):
        w = self.workspace()
        w.branch("io")
        with w.mount("io") as p:
            with open(p / "sparse", "w+b") as f:
                f.seek(128 * 1024); f.write(b"end"); f.flush()
                f.seek(128 * 1024 - 2); self.assertEqual(f.read(), b"\0\0end")
                f.truncate(1); f.truncate(128 * 1024 + 3); f.seek(128 * 1024)
                self.assertEqual(f.read(), b"\0\0\0")
        w.accept("io")

    def test_explicit_timestamps_and_permissions(self):
        w = self.workspace({"file": b"hello"})
        w.branch("meta")
        with w.mount("meta") as p:
            os.chmod(p / "file", 0o640)
            os.utime(p / "file", ns=(1_600_000_000_000_000_000, 1_650_000_000_000_000_000))
            stat = os.stat(p / "file")
            self.assertEqual(stat.st_mode & 0o777, 0o640)
            self.assertEqual(stat.st_atime_ns, 1_600_000_000_000_000_000)
            self.assertEqual(stat.st_mtime_ns, 1_650_000_000_000_000_000)
        w.accept("meta")

    def test_generated_operations_match_native_filesystem(self):
        w = self.workspace()
        native = w.root / "native"
        native.mkdir()
        w.branch("oracle")
        rng = random.Random(4938251)
        operations = 1200
        def snapshot(root):
            return {p.name: (p.read_bytes(), p.stat().st_mode & 0o777, p.stat().st_nlink) for p in root.iterdir()}
        def apply(root, op, a, b, offset, data):
            try:
                if op == 0:
                    (root / a).write_bytes(data)
                elif op == 1:
                    with open(root / a, "r+b") as f:
                        f.seek(offset); f.write(data)
                elif op == 2:
                    os.truncate(root / a, offset)
                elif op == 3:
                    os.replace(root / a, root / b)
                elif op == 4:
                    os.link(root / a, root / b)
                elif op == 5:
                    os.unlink(root / a)
                elif op == 6:
                    with open(root / a, "ab") as f:
                        f.write(data)
                elif op == 7:
                    os.chmod(root / a, 0o600 if offset % 2 else 0o644)
                return None
            except OSError as e:
                return e.errno
        with w.mount("oracle") as p:
            for i in range(operations):
                op = rng.randrange(8)
                a, b = (f"file-{rng.randrange(12)}" for _ in range(2))
                offset = rng.randrange(140_000)
                data = rng.randbytes(rng.randrange(1, 200))
                expected = apply(native, op, a, b, offset, data)
                actual = apply(p, op, a, b, offset, data)
                self.assertEqual(actual, expected, f"operation {i}: {op}, {a}, {b}")
                if i % 25 == 0:
                    self.assertEqual(snapshot(p), snapshot(native), f"state after operation {i}")
            self.assertEqual(snapshot(p), snapshot(native))
        w.accept("oracle")

    def test_killed_acceptance_is_all_or_nothing(self):
        for point in ("after_versions", "after_head", "after_receipt", "after_commit"):
            with self.subTest(point=point):
                w = self.workspace({"a": b"old-a", "b": b"old-b"})
                w.branch("crash")
                with w.mount("crash") as p:
                    (p / "a").write_bytes(b"new-a")
                    (p / "b").write_bytes(b"new-b")
                w.call("seal", "crash")
                env = dict(os.environ, WITNESS_FAULT_POINT=point)
                child = subprocess.Popen(["witnessfs-fault", "--store", str(w.store), "accept", "crash"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                selector = selectors.DefaultSelector()
                selector.register(child.stderr, selectors.EVENT_READ)
                try:
                    self.assertTrue(selector.select(10), f"failpoint not reached: {point}")
                    marker = child.stderr.readline().decode()
                    self.assertEqual(marker.strip(), f"WITNESS_FAILPOINT {point}")
                    child.kill()
                    child.communicate(timeout=10)
                finally:
                    if child.poll() is None:
                        child.kill(); child.communicate()
                    selector.close()
                self.assertEqual(w.call("check"), "ok\n")
                out = w.root / "recovered"
                w.call("export", out)
                prefix = b"new" if point == "after_commit" else b"old"
                self.assertEqual((out / "a").read_bytes(), prefix + b"-a")
                self.assertEqual((out / "b").read_bytes(), prefix + b"-b")
                if point == "after_commit":
                    self.assertEqual(w.call("receipt", "crash")["outcome"], "accepted")
                else:
                    w.call("receipt", "crash", expected=1)
                # Repeating acceptance after a crash either commits once or returns
                # the durable receipt; the head must end at revision 2 in both cases.
                w.call("accept", "crash")
                self.assertEqual(w.call("stats")["head"], 2)


class RecordingResult(unittest.TextTestResult):
    def startTest(self, test):
        self.started = time.perf_counter()
        super().startTest(test)
    def addSuccess(self, test):
        results.append({"test": test.id(), "outcome": "passed", "seconds": time.perf_counter() - self.started})
        super().addSuccess(test)
    def addFailure(self, test, err):
        results.append({"test": test.id(), "outcome": "failed", "error": str(err[1])})
        super().addFailure(test, err)
    def addError(self, test, err):
        results.append({"test": test.id(), "outcome": "error", "error": str(err[1])})
        super().addError(test, err)


results = []
if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(KernelTests)
    run = unittest.TextTestRunner(verbosity=2, resultclass=RecordingResult).run(suite)
    output = Path(os.environ.get("WITNESS_RESULTS", "/tmp/witness-fuse-results.json"))
    output.write_text(json.dumps({"format": "witnessfs.kernel-tests.v1", "platform": platform.platform(), "tests": results, "passed": run.wasSuccessful()}, indent=2) + "\n")
    sys.exit(0 if run.wasSuccessful() else 1)
