# WitnessFS: observed dependencies at the workspace boundary

**Proposal:** put a conservative dependency validator underneath ordinary coding
tools, then test whether it catches invalidated work cheaply enough to be useful
before integration.

The problem is a missing edge. Task A changes a configuration's units. Task B
read the original unit and adds a consumer in another file. Both pass their
private tests. Their text merge succeeds, but the combined program is wrong.
The files changed by B do not reveal the configuration it relied on.

WitnessFS records that dependency through Linux FUSE. It also records negative
lookups, directory listings, metadata and symlink targets. No tool wrapper or
agent-declared read list is required for supported operations inside the mount.
The validator compares these facts with current state before publication.

## What exists

The Rust implementation has versioned namespace/inode keys, private overlays,
deduplicated chunks, immutable snapshots and machine-readable decisions. A
per-branch process lease prevents a mount from racing sealing or acceptance.
Validation, publication and the receipt share a SQLite FULL/WAL transaction.
Failed filesystem operations retain their observed inputs without retaining
their partial writes. Readable-open observations cover cached and mmap reads.

The companion demo replays actual Linux/Git commands. A timeout drops from 60
seconds to 60 milliseconds after a clean Git merge. WitnessFS rejects the stale
branch, names the changed configuration and accepts a fresh corrected rerun.
The same recording includes absent-path and directory dependencies, independent
accepted writes and an intentional false rejection from a comment edit.

Verification includes 640 generated transactions against an independent serial
interpreter, 1,200 file operations against native Linux, 17 mounted tests and
four forced process kills at acceptance boundaries. The checks establish bounded
evidence for the implemented contract, not application correctness.

## Adoption path

The first useful pilot is a **disposable agent workspace runner**:

1. Import a fixed source snapshot without mutable `.git` state or caches.
2. Give each task a private mount and run its normal tools there.
3. Stop the task, unmount, seal and validate. On rejection, inspect the receipt
   and rerun with fresh inputs. On acceptance, export into ordinary review/CI.
4. Measure useful accepted work, rerun cost, dependency breadth and I/O overhead.

This requires no replacement of a source-control system. A first deployment
should remain advisory or isolated from production acceptance until a realistic
workload establishes its value. Pinned tools and an OS sandbox are complementary
requirements; external services and prior conversational knowledge are untracked.

Potential reviewers are engineers working on agent workspaces, developer
infrastructure, source control and build/test systems. Anthropic's
[Scion write-up](https://www.anthropic.com/engineering/redesigning-source-control)
documents the broader pressure from agent-scale change volume and concurrent
integration. That is topical evidence, not evidence that Scion lacks a particular
feature or that its authors want this component. No internal interface was
available to integrate or benchmark against.

## Why this experiment rather than another agent UI

The value is a concrete operating-system boundary, a falsifiable contract and a
failure harness. It makes the engineering questions reviewable: what gets
observed, how caching interacts with observation, what can race acceptance, how
identity survives rename and unlink, and what a crash leaves visible.

This is not a new concurrency-control algorithm. Zit already explores agent
transactions with declared/inferred symbol reads. BranchFS, AgentFS and Factory
VFS cover neighboring filesystem/workspace problems. WitnessFS chooses
transparent filesystem reads and conservative whole-file validation; its
distinctiveness is in that integration and evidence, not priority of invention.

## Costs and next experiment

In the recorded 10,000-file run, snapshot CLI median was 2.97 ms and import took
1.12 seconds. Mount/unmount added 21.70 ms. Warm 4 MiB reads took 12.10 ms versus
0.26 ms natively; 64 KiB replacement plus fsync took 1.14 ms versus 0.38 ms.
These are local VM measurements with unmatched storage engines. No distributed
scaling or production-throughput claim is made.

The most important unanswered question is **false rejection on real agent
workloads**. A broad search may observe thousands of unrelated inputs. The best
next step is a small corpus of parallel tasks with independently reviewed
outcomes, followed by measurement of useful work saved versus rerun and I/O
cost. Byte-range or symbol-aware refinement should be justified by that evidence.
No maintainer/adopter interest has yet been obtained.
