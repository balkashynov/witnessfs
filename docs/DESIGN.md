# WitnessFS: design contract

WitnessFS records filesystem observations made by ordinary tools in private,
versioned workspaces. A branch is one transaction. Acceptance validates its
observations and write preimages against the current committed state, then
publishes its changes atomically. The intended users are people running parallel
coding agents and the runtimes that host them.

## Claim and boundaries

The target property is equivalence to execution in acceptance order **for the
supported observations inside the mounted filesystem**. This does not prove
program correctness or capture a model's reasoning. Reads from previous agent
sessions, network services, host files, environment variables, clocks and external
databases are outside that property. All relevant workspace reads must occur in
the same branch. WitnessFS is not a security sandbox.

Validation is deliberately conservative. Reading one byte initially makes the
whole file a dependency. A changed input can reject a still-correct program.
There is no language model or probabilistic decision in acceptance.

## Storage and lifecycle

SQLite WAL stores versioned namespace entries and inodes, content-addressed 64 KiB
chunks, branch overlays, observations and acceptance receipts. A snapshot is a
revision number, so branch creation does not copy the tree. Namespace entries
point to stable inode IDs, making rename and open-file lifetimes explicit.
Published versions remain immutable. Garbage collection is outside the initial
release.

Each operation records observations in the same database transaction as its
effect. `FULL` durability is the initial default. Only a sealed, unmounted branch
can be accepted. A per-branch process lock excludes a live mount from sealing or
acceptance; a SQLite write transaction serializes validation with publication.
Conflicts preserve the branch's work and produce a machine-readable receipt.

The FUSE layer disables entry/attribute caching and observes all file content on
every readable open, covering kernel-cached and memory-mapped reads. Each branch
has its own mount and exclusive process lease. The kernel writeback-cache
capability is not enabled.
Lookup failures, directory enumeration, metadata, symlink targets and file bytes
have separate observation kinds. Namespace writes record preimages even when the
program never reads them. Observations satisfied by the branch's own writes are
covered by those write preimages. Unsupported operations return an error.

Directory metadata is stable except when explicitly changed; child membership is
observed through lookup and enumeration. This is an intentional virtual-filesystem
semantic to avoid making every root lookup depend on every unrelated creation.

## Evaluation

1. A small independent serial-execution oracle exercises generated transactions,
   including absent paths and directory membership.
2. Real Linux FUSE tests exercise Bash, Python, rename, unlink with open handles,
   directory phantoms, and parallel commits.
3. Crash tests kill processes around acceptance and check all-or-nothing recovery.
4. A failing program demonstrates a conflict-free Git merge and a rejected stale
   filesystem transaction. The demonstration uses real commands, not an animation
   presented as execution.
5. Benchmarks report branch creation, acceptance, read/write overhead and storage.
   The demo includes a conservative false rejection. There is no production
   throughput claim.

These checks are implemented. See [validation](VALIDATION.md) for measured results
and [the correctness contract](CORRECTNESS.md) for the scoped argument and limits.

## Prior art and positioning

- [Anthropic Scion](https://www.anthropic.com/engineering/redesigning-source-control)
  describes source-control and CI pressure at very high merge rates. WitnessFS is
  an experimental workspace component, not a replacement for Scion or evidence
  that Anthropic needs this implementation.
- [Zit](https://getzit.org/science) already applies optimistic transactions to
  agents using declared and syntactically inferred reads. WitnessFS investigates
  observed filesystem dependencies instead. Optimistic concurrency is not novel.
- [BranchFS](https://github.com/multikernel/branchfs) supplies copy-on-write branch
  contexts. Its current README describes snapshot isolation and parent commits;
  its original paper's first-commit-wins description is older. Do not conflate them.
- [AgentFS](https://github.com/tursodatabase/agentfs) and
  [Factory VFS](https://github.com/Factory-AI/vfs) provide persistent virtual agent
  filesystems. SQLite-backed storage and FUSE are established engineering choices.
- [CoAgent](https://arxiv.org/abs/2606.15376) investigates agent concurrency with
  semantic conflict judgment and repair. WitnessFS uses conservative byte and
  namespace observations and deterministic acceptance.
- Transactional filesystems, database serializability and dynamic build dependency
  tracking predate this project. Any distinctiveness claim concerns the tested
  integration, not invention of these ideas.

Sources checked 2026-10-15. No maintainer or Anthropic endorsement has been obtained.
