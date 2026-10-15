# What acceptance means

## Contract

The directly implemented guarantee is: **at the instant an acceptance transaction
publishes a branch, every recorded base-state dependency and write preimage
matches the immediately preceding committed revision.** The branch is sealed and
unmounted first. No other acceptance can interleave between validation and
publication. A mismatch publishes none of its writes and saves a rejection.

Under complete observation of the relevant filesystem inputs, the standard
optimistic-concurrency argument gives equivalence to applying the accepted work
in acceptance order. This is a scoped argument, not a machine-checked proof or
deterministic replay of an entire process. Fresh inode allocation and wall-clock
timestamps are external choices, not replayed outputs. Symbolic meaning,
application invariants and whether an agent understood a file are outside it.

## State model

- A committed revision is an immutable key/value state. Each namespace entry
  `d/{parent}/{raw-name-hex}` maps to an inode; each `n/{inode}` holds its metadata
  and content references. Deleted entries are tombstones.
- Branch creation stores the current revision. A private overlay shadows that
  revision. New inodes receive globally unique IDs; branches never accidentally
  allocate the same object identity.
- A content dependency fingerprints file kind, logical size, chunk map and
  symlink target. Chunks are SHA-256 addressed. A metadata dependency includes
  every nonconstant node field exposed by `getattr`, including allocated blocks.
- A lookup dependency records the namespace entry, including its absence.
  Enumeration fingerprints the full base membership of that directory.
- Every modified key records its base value, whether or not the application
  explicitly read it. Reads of an already modified key can be omitted because
  the write preimage is stronger. Directory observations retain the full base
  membership even after local additions, a conservative choice.

## Argument by induction

Suppose accepted branches through revision H have been applied in order. Take
the next sealed branch B. For each observation of base state, acceptance compares
its saved fingerprint with H. If any differs, B remains private. If all match,
the base facts B used are still true at H, modulo hash collisions. Its own
intermediate writes are already part of its private execution. All keys B writes
also match their preimages, so applying the overlay cannot overwrite a concurrent
change to those keys. The new state and receipt become visible together at H+1.

This prevents stale reads and blind lost updates within the modeled key space.
Checking absence prevents a decision based on a missing file from passing after
that name appears. Checking directory membership prevents a catalog derived from
an old listing from passing after a new member appears. Changes that revert to
the same observed value are allowed where the fingerprint is equal; validation
checks current facts, not every intervening historical event.

The hypothesis depends on capturing relevant operations. FUSE lookup/getattr
responses use zero TTL. Every readable open observes content before the kernel
can satisfy reads from cache or mmap. Directory open captures a stable listing
and records membership. Only one mount may own a branch, and CLI operations use
the same branch lease. The kernel writeback-cache capability is not enabled.
An ordinary unmount must complete before sealing; tests exercise mmap flush.

## Atomicity and recovery

Each filesystem operation uses a database transaction. A savepoint protects its
writes, while observations are collected separately and saved even if the
operation fails. An error returned to a program is still information that can
influence later work.

Acceptance uses `BEGIN IMMEDIATE`, validates all observations, inserts versions,
advances head, stores the receipt and changes branch state before one commit.
SQLite uses WAL and `synchronous=FULL`. The test-only build stops at four cuts:
after versions, after head, after receipt and after commit. The harness sends
SIGKILL, reopens the database, checks integrity, exports both files, and retries
acceptance. Before commit it observes the complete old state; after commit the
complete new state and saved receipt. It never publishes the same decision twice.

This tests process crashes at those cuts. It does not test power failure,
storage firmware, disk-full handling at every write, or every SQLite failure.
SQLite's structural integrity check is not a complete content/reference audit.
Receipts are local diagnostic records, not signed proofs for an untrusted party.

## Supported surface and deliberate differences

Regular files, directories, symlinks, hard links, read/write/append/truncate,
rename/replace/RENAME_NOREPLACE, lookup, enumeration, stat, chmod/chown and ordinary
timestamp updates are implemented. Raw non-UTF-8 names are retained. Symlinks are
resolved by the kernel; a target outside the mount is outside observation.
Pre-epoch timestamps are rejected. Import preserves file mode and modification
time, but assigns the importing process's ownership. Export targets a new
directory and preserves content, mode, symlinks and hard-link identity.

Directories intentionally keep their metadata stable across membership changes;
enumeration and lookup supply membership dependencies. Their link counts do not
model Unix's subdirectory-count convention. Reads do not update atime. Block
counts report logical allocated chunks, not SQLite physical pages. `statfs`
returns the FUSE default capacity values, which must not guide workload decisions.
Advisory kernel locks are local to a mount, not distributed transactions.

Extended attributes return absent/unsupported responses. Devices, sockets, FIFOs,
RENAME_EXCHANGE and other advanced operations are unsupported. Unsupported paths
must not be silently assumed to have the native filesystem's semantics. The
public Rust module is implementation machinery; callers that bypass observation
methods using raw node access are outside the mounted-filesystem contract.

## Explicit counterexample to semantic precision

Task B reads a file containing a setting and a comment. It uses only the setting.
Task A changes only the comment and is accepted. B is rejected because its
whole-file content dependency changed. This rejection is reproducible in the demo.
Finer byte-range or language-aware dependencies are future research; blindly
discarding reads would weaken the contract. Broad source searches and test runs
can cause many valid changes to conflict, so representative agent workloads are
the next evaluation requirement.
