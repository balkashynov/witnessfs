# Validation record

Recorded 2026-10-15. The implementation is an experimental local filesystem.
No production deployment, Anthropic integration or external adopter result is
claimed. See the hosted-check update below and `evidence/source-manifest.json` for the source and
binary hashes associated with these records.

## Environment

- Host: Apple M3 Pro, 12 logical CPUs, 18 GB RAM, macOS arm64.
- Native core checks: Rust/Cargo 1.93.0, formatting, clippy with warnings denied,
  and all Rust test targets.
- Mounted checks: Docker Desktop, Linux 6.10.14-linuxkit aarch64, Debian bookworm,
  approximately 8 GB VM RAM. Build image's Rust 1.93 tag resolved to 1.93.1.
- Test containers: network disabled, `/dev/fuse` and `SYS_ADMIN` for the mount.
  No privileged container or live company repository was used.
- Linux-specific formatting, clippy with warnings denied, compilation and Rust
  tests passed in the final Docker build before the recorded kernel, demo and
  benchmark run. The final cache optimization only changes Linux mounting code;
  the native macOS core is unchanged from its recorded test run.

## Correctness evidence

| Check | Result | Scope |
| --- | --- | --- |
| Core transaction tests | 8 passed | Write skew, independent writes, namespace dependencies, error rollback, sparse files, object identity, branch leases and concurrent acceptance |
| Independent serial interpreter | 80 histories / 640 transactions | 406 accepted, 234 rejected; accepted outputs and observations agree with serial application |
| Blind-composition negative control | 240 changed observations | Shows that the generated inputs actually exercise stale decisions; this is a count of changed observations, not a count of failing applications |
| Linux kernel suite | 18 passed | Real mount operations, not mocked FUSE callbacks |
| Native filesystem differential | 1,200 seeded operations | Compare errors, content, file mode and file hard-link counts at checkpoints; directory timestamp semantics are deliberately different |
| Acceptance crash cuts | 4 passed | SIGKILL after versions/head/receipt/commit; all-or-nothing state, integrity and idempotent decision recovery |
| Demo | Passed all assertions | Clean Git merge breaks the combined retry test; WitnessFS rejects stale work; fresh retry and exported tests pass |
| Conservative rejection | Reproduced | Changing only a comment rejects a reader that uses only the setting |

The kernel suite also covers missing-file reads, directory phantoms, cached mmap
reads, flushed shared mmap writes, non-UTF-8 names, symlinks, compiled GCC
executables, open-unlink behavior, hard links, rename/replace/RENAME_NOREPLACE,
explicit timestamps and out-of-band access while a branch is mounted.

The serial oracle has its own plain-map interpreter. It compares against current
serial state after each accepted transaction; it does not ask WitnessFS's
validator whether a result should be right. It is bounded randomized testing,
not exhaustive state-space exploration or a formal proof.

Raw evidence:

- [Kernel test cases and outcomes](evidence/kernel-tests.json)
- [Actual command output, observations and receipts](evidence/demo.json)
- [Timing samples and storage counts](evidence/benchmark.json)
- [Native Rust output](evidence/rust-macos.txt)
- [Interactive replay screenshot](evidence/demo-desktop.jpg)

## Measurements

One local run. Values below are medians except single-sample initialization.
All samples, minimum/maximum values and environmental caveats are retained in
the raw JSON. Host load and warm caches are not controlled across machines or
runs. Earlier development runs varied; these numbers are not guarantees.

| Operation | Samples | 100 files | 10,000 files |
| --- | ---: | ---: | ---: |
| WitnessFS snapshot CLI | 25 | 1.90 ms | 3.19 ms |
| Git detached worktree | 8 | 4.64 ms | 213.03 ms |
| WitnessFS mount + unmount | 8 | 14.42 ms | 14.89 ms |
| WitnessFS import | 1 | 23.41 ms | 1,041.14 ms |
| Git add + first commit | 1 | 12.22 ms | 556.88 ms |
| Accept one newly written output file | 10 | 2.31 ms | 2.25 ms |

Snapshot creation stores a revision; Git worktree creation materializes files.
They are **not equivalent completed checkouts**. The comparison exposes that
tradeoff, with mount and import costs alongside it. All CLI measurements include
process startup and SQLite open/close. Acceptance samples each use a new branch
and include no retries or concurrent acceptance load.

| I/O operation | Native filesystem | WitnessFS mount | Samples |
| --- | ---: | ---: | ---: |
| Read/compare 4 MiB, warm caches | 0.292 ms | 0.867 ms | 25 each |
| Replace 64 KiB and fsync | 0.425 ms | 1.104 ms | 25 each |

Warm reads were **3.0x slower** through the mount in this run. Each sample opens,
reads, checks equality and closes; both paths are warmed first. A readable open
records whole-file content even when kernel caching reduces read callbacks.
Writes use the same 64 KiB random payload and fsync on both paths. The native
baseline is the container writable filesystem, while WitnessFS is a SQLite
filesystem on top of it. This is useful end-to-end cost evidence, not an isolated
FUSE or SQLite microbenchmark and not a matched storage-engine comparison.

### Cache optimization

The first recorded implementation (`3db2521`) invalidated the data cache on each
open. Its warm 4 MiB read took 12.104 ms versus 0.256 ms natively. The final
implementation retains that cache with `FOPEN_KEEP_CACHE`, while still recording
content on every readable open. The per-branch lease excludes another mount or
CLI writer. This reduced the observed median to 0.867 ms; host timing varies,
so the roughly 14x reduction is a local before/after result, not a universal
speedup. [The earlier raw samples](evidence/benchmark-before-cache.json) are kept.

The added regression warms the cache, writes through a hard-link alias, modifies
a shared mmap, truncates and extends the file, then remounts. Every read must show
the current bytes. All prior stale-read, mmap, native-differential and crash tests
also pass with the retained cache.

Ten thousand unique 2 KiB files contain 20,480,000 bytes of source content.
The initialized WitnessFS directory occupied 59,092,992 logical file bytes,
including indexes, serialized metadata and retained import observations/overlay.
Twenty-five additional snapshots added no content blobs or version rows.
The database reused existing free pages, so its measured length was unchanged;
this does not imply snapshots need zero storage forever. There is no GC.

The implementation should next be evaluated on representative agent read sets:
conflicts per useful change, rerun cost, whole-project searches and dependency
caches. The small generated histories do not measure those user-facing outcomes.

## Reproduction

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --nocapture
bash scripts/verify.sh --benchmark
```

The Docker build runs the Rust checks. `verify.sh` runs the real kernel suite and
records the demo; `--benchmark` adds measurements. Outputs go into ignored
`.runs/` and do not overwrite the committed evidence. Timing and timestamps will
change. Repeated demo runs should preserve outcomes, conflict kinds and paths;
metadata hashes can differ because real filesystem timestamps are used.

## Hosted checks and public presentation

Published 2026-10-15 (America/Edmonton):
[source](https://github.com/balkashynov/witnessfs) and
[interactive recorded demo](https://balkashynov.github.io/witnessfs/).

[GitHub Actions run 38845047351](https://github.com/balkashynov/witnessfs/actions/runs/38845047351)
passed at `a06b6a9973b623fa2b8c0e0e9d4f103caa22191f`: Rust formatting,
clippy and tests on macOS and Ubuntu, plus all 18 actual Linux FUSE tests,
the four acceptance crash cuts and the scripted Git/failure/recovery demo.
The kernel suite runs as the ordinary runner user in GitHub's disposable Linux
VM. The first Docker-based hosted attempt was denied permission to mount;
the native runner path avoids changing host security policy.

This adds hosted Linux x86_64 correctness evidence. The published timing samples
remain the original local ARM64 measurements above; they were not replaced with
CI timings. The later artifact-upload correction includes only `.runs/*.json`
from the hidden results directory. Private notes and contacts are excluded.

The public README, source links, five replay steps, four secondary cases,
reloadable step/case links, desktop and 390px phone layouts were checked.
The deployed HTML, JavaScript, CSS, icon and three JSON records matched the local
files byte for byte. An isolated missing-data preview showed the error notice
and disabled replay controls. The valid public demo produced no console errors.

## Browser verification

The local evidence page was exercised in the Codex browser at its desktop
viewport and a 390-pixel mobile viewport. All five history steps, four secondary
cases, the final exported test output and benchmark records were inspected.
No browser warning/error logs were present. The mobile page had no horizontal
overflow in the inspected DOM. The viewport override was reset afterward.

## Remaining limits

No real Claude session, production incident, full POSIX certification, power-loss
test, distributed failure test, sustained multi-process load test or demonstrated
hiring outcome. External effects remain untracked. Whole-file dependencies can
reject valid work. The current global SQLite writer and retained history are
scalability constraints. The [contract](CORRECTNESS.md) states intentional
filesystem differences and unsupported operations.
