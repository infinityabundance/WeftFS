# Verification — model checking and power-loss evidence

This directory contains two independent kinds of evidence for WeftFS v1:

| Artifact | Kind | What it establishes | What it does **not** establish |
|---|---|---|---|
| `tla/WeftCommit.tla` + `tla/WeftCommit.cfg` | bounded TLC model checking | the **abstract** commit/recovery state machine has the listed invariants for the checked constants | that the Rust or Go implementation refines the model |
| `tla/WeftCommitBuggy.tla` | deliberately flawed model | TLC finds a concrete counterexample for a plausible recovery defect | that the shipped code has (or lacks) any other defect |
| `scripts/powerloss_court.sh` | QEMU VM power-cut proxy | the shipped Rust + Go stores recover the same exact whole state after a guest power cut at a random point in the write loop | physical media/device power loss, host page-cache loss, or an fsync that lies |

No claim in this directory is a proof about source code. "Model-checked"
means the named TLC run completed with no invariant violation over its bounded
state space. The implementation claims remain test- and receipt-backed.

---

## 1. TLA+ model checking

### 1.1 Abstraction mapping

`WeftCommit.tla` models one store, one writer, and bounded generation number.
Each TLA+ variable maps to a real artifact as follows.

| TLA+ | Implementation | Code |
|---|---|---|
| `commit[g] ∈ {"none","torn","valid"}` | `commits/<g>.commit` absent, torn (CRC/WCB validation fails), or valid | `src/recovery.rs::load_commit_file`, `src/recovery.rs::recover` |
| `slot[s]` (`0` empty, `1..MaxGen` generation, `MaxGen+1` torn) | 4096-byte superblock slot; CRC32C at offset 64; generation, commit hash, length | `src/format.rs` superblock encode/decode, `src/recovery.rs::read_superblock` |
| `durableObj` | object/manifest blobs after `tmp/` write, fsync, rename, and directory fsync | `src/store.rs::store_blob` |
| `Start`/`SyncObjects` | apply ops, write blobs, fsync them | `src/store.rs::commit_ops` (`crash_point("after_objects")`) |
| `WriteCommit`/`SyncCommit` | create `commits/<g>.commit`, write, fsync, fsync `commits/` | `src/store.rs::publish_commit` |
| `WriteSlot`/`SyncSlot` | pwrite the alternating superblock slot, fsync `superblock` | `src/store.rs::publish_commit` |
| `Return` | fsync the store root, then return success to the caller | `src/store.rs::publish_commit` |
| `Crash` | power cut between any two actions; media keeps completed fsyncs | — |
| `CrashCommitWrite` | torn commit file from a cut during the write | `load_commit_file` CRC/hash rejection |
| `CrashSlotWrite` | torn superblock slot from a cut during the pwrite | `read_superblock` CRC rejection |
| `MaxVisible` | highest generation in a valid slot | `recover()` `max_visible` |
| `Candidate(g)` | `g <= max_visible`, commit record verifies, full chain loads, referenced objects exist | `recover()` + `load_state_at` |
| `RecoverStep`/`ReplayStep` | scan downward, then replay commits 1..selected | `recover()` + `load_state_at` |
| `NoPartial` (I5) | no commit prefix or mixed state is exposed | spec §7 I5 |
| `NoLostDependency` (I6) | every replayed generation has all its objects | spec §7 I6, `validate_catalog` |
| `Durability` | a commit whose call returned survives a later cut | spec §4 |
| `HighestValid` | recovery selects the highest generation passing every check and falls back otherwise | spec §2.1, §8 |
| `ReplayIdempotent` | a second recovery pass over the same media yields the same state | spec §4 |

The Go implementation is an independent implementation of the same on-disk
format; the model maps to the format contract, not to one language's internals.
The power-loss court checks that both implementations agree on real crashed
images.

### 1.2 Modelled crash points and assumptions

The model's crash points mirror `publish_commit`'s order:

```
objects+checkpoint blobs durable
  -> commit file written+fsynced, commits/ fsynced
  -> superblock slot written+fsynced
  -> store root fsynced, commit returns
```

A cut can happen between any two actions. A cut during the commit-file write or
the in-place superblock write leaves a torn artifact whose CRC fails. The model
assumes:

- fsync is honoured: a synced write is complete and durable, and a cut cannot
  lose it;
- an unsynced write may be lost or torn;
- object blobs are atomic (`tmp/` + fsync + rename + directory fsync), so a
  final object is either present or absent, never half-written;
- commit files are immutable once written; there is no hash collision;
- there is one writer; GC never deletes a live object;
- checkpoints are **not** modelled (see limitations);
- selectors, capabilities, contexts, and authorization are **not** modelled.

`MaxGen=8` is the fixed model's bounded constant; two operations per commit
make a partial commit observable as a state that is neither the old nor the new
whole state. The deliberately flawed model uses `MaxGen=3`, which is enough to
expose the defect.

### 1.3 Checked properties

TLC checked `TypeOK`, `NoPartial`, `HighestValid`, `NoLostDependency`,
`Durability`, `ReturnedObjectsDurable`, and `ReplayIdempotent` on the corrected
model. Result (receipt `bench/receipts/tla-20261103T223957Z.json`):

| Model | Constants | States generated | Distinct | Depth | Result |
|---|---|---:|---:|---:|---|
| `WeftCommit` | `MaxGen=8` | 615 | 591 | 68 | pass, no error |
| `WeftCommitBuggy` | `MaxGen=3` | 32 | 29 | 9 | expected violation of `NoPartial` |

The flawed model's counterexample is preserved in
`tla/WeftCommitBuggy.counterexample.txt`: a cut while writing generation 1's
commit file leaves a torn file; a recovery that trusts the highest commit file
without CRC/superblock checks replays one operation of that commit and exposes
a partial state. The corrected model never accepts a non-`valid` commit.

**Limitations of the model check.** The state space is bounded and the
abstraction omits checkpoints (including the "skip the checkpoint commit's ops"
rule), GC, capability filtering, selector evaluation, and multi-writer
behaviour. There is no refinement proof linking the TLA+ actions to Rust or Go
source. `tests/bounded_recovery.rs` and the crash tests remain the
implementation-level evidence.

### 1.4 Reproduce

```
scripts/run_tla.sh
```

The script self-dispatches into the `tla` Docker court (hard limits: 3 GiB
memory/swap, 4 CPUs, 1024 pids, 1 GiB bounded `/tmp`; no-new-privileges;
cap-drop=ALL). It downloads nothing at run time: `docker/tla.Dockerfile` pins
TLC 1.7.4's `tla2tools.jar` by sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
It writes `bench/receipts/tla-<UTC>.json` and a raw TLC log.

---

## 2. Power-loss court (QEMU VM power-cut proxy)

### 2.1 Rig

`scripts/powerloss_court.sh` runs entirely inside the `vm` Docker court. The
harness builds both CLIs from the mounted tree, then:

1. builds a read-only raw ext4 root disk containing a static busybox, the Rust
   CLI, the static Go CLI, and `verification/powerloss/guest-init.sh` as PID 1;
2. creates a fresh qcow2 overlay on an empty raw ext4 base for every iteration;
3. boots a Debian kernel/initramfs with QEMU (KVM when `/dev/kvm` is available,
   TCG fallback otherwise), with a read-only root and the qcow2 data disk;
4. the guest initialises the store once, creates `/f` (1 MiB), and then writes
   64 KiB at offset 0 in a loop, one commit per write, printing
   `COMMIT i=<n> gen=<g>` only after the CLI returns. The guest never calls
   `sync(1)`;
5. the host waits for at least one returned commit, sleeps a seeded
   pseudo-random delay, then sends `SIGKILL` to `qemu-system-x86_64` with no
   guest shutdown and no cache flush;
6. boots the same qcow2 image again; the guest mounts it (kernel ext4 journal
   replay only, **no fsck**), then runs Rust `info`, Rust `verify --full`, Go
   `info`, Go `verify --full`, and reads `/f` with both implementations;
7. the host compares both recovered generations, snapshot roots, and file
   bytes against the exact expected content for generation `g` (init = gen 1,
   create = gen 2, write *n* = gen *n*+2), and checks that every returned
   commit from the write log is still present.

The rig includes a control that distinguishes a VM power cut from a process
abort: after each returned commit the guest appends one byte to
`/data/unsynced-control` on the data filesystem, **never fsynced**. In all
completed runs the file's post-reboot size was 0, while the fsynced store
commits survived. This demonstrates that the rig discarded the guest page
cache, which a mere writer-process abort would not do.

### 2.2 Explicit limits and claim

Hard container limits are inherited from `scripts/docker-court.sh`: 3 GiB
memory, 3 GiB memory+swap, 4 CPUs, 1024 pids, 1 GiB `nosuid,nodev` `/tmp`,
`no-new-privileges`, cap-drop=ALL. Additional rig limits: guest 512 MiB,
read-only root 160 MiB, data disk 128 MiB, bounded boot/verify timeouts, one
writer, and a bounded iteration count. QEMU runs with `cache=none` for the
published run (O_DIRECT on the qcow2 overlay).

**Claim:** this is a **QEMU virtual-machine power-cut proxy**. `SIGKILL` loses
the guest page cache and QEMU's user-space state. It does **not** physically
remove host page cache, the host disk cache, or a real device's write cache; it
is not a physical power-loss test. A software process abort alone would not be
accepted as this evidence.

### 2.3 Results

Published primary run (receipt
`bench/receipts/powerloss-20261103T224003Z.json`): 30 iterations, KVM,
`cache=none`, guest kernel 6.1.0-54-amd64, Debian initramfs, ext4
`data=ordered,barrier=1`, no fsck.

| Metric | Value |
|---|---:|
| Iterations | 30 |
| Pass | 30 |
| Fail | 0 |
| Old state (`recovered == last returned`) | 26 |
| New state (`recovered > last returned`) | 4 |
| Partial / mismatched state | 0 |
| Durability violations | 0 |
| Rust/Go disagreements | 0 |
| Iterations with a documented unpublished tail | 1 |
| Unsynced-control loss observed | 30 / 30 |

The secondary run (receipt
`bench/receipts/powerloss-20261103T224205Z.json`) used the same rig with
`cache=writeback`: 10 iterations, 10 pass, 9 old, 1 new, 0 partial, 0
durability failures, 0 Rust/Go disagreements, 2 documented unpublished tails,
and unsynced-control loss in 10/10 iterations. Iteration 29 of the primary run
is the direct exercise of the unpublished-tail acceptance path: both verifiers
reported exactly the documented `above the highest published superblock
generation` note for generation 58, both recovered generation 57 with the same
root, and both read the exact expected bytes.

The earlier 20-iteration and 12-iteration runs and the single-iteration smoke
tests are retained in `bench/receipts/` as run history. One pre-fix
12-iteration run (`powerloss-20261103T223301Z`) flagged a valid-but-unpublished
commit tail as a verifier "problem". Recovery was correct and both
implementations returned the same exact state; the court's acceptance
predicate was then narrowed to accept only (a) the documented `above the
highest published superblock generation` diagnostic and (b) an unparseable
commit file at a generation strictly above the recovered generation. Any other
verify problem fails the iteration. The classification was validated against
the retained raw verify log from that run; the published primary run then
exercised the accepted-tail path in iteration 29.

### 2.4 Reproduce

```
scripts/powerloss_court.sh
# or, with explicit parameters inside the court:
scripts/docker-court.sh vm "POWERLOSS_ITERATIONS=30 POWERLOSS_CACHE_MODE=none bash scripts/powerloss_court.sh"
```

Receipts record the exact kernel/initramfs, QEMU version, acceleration mode,
cache mode, seeded kill delays, per-iteration generations, verifier problems,
durability result, unsynced-control result, and raw serial logs.

### 2.5 What remains

- **Physical power loss** (real media/device write-cache loss) is not
  achievable on this host inside Docker. No workaround here is relabelled as
  physical.
- Host page-cache loss and an fsync that lies to the guest are not modelled by
  this rig.
- The 10,000-iteration campaign from the F5 contract is supported
  (`POWERLOSS_ITERATIONS`) but only bounded runs have been executed.
- Checkpoints are exercised by the guest writes (interval 5) but are not in the
  TLA+ abstraction.

---

## 3. Evidence index

| Artifact | Result |
|---|---|
| `bench/receipts/tla-20261103T223957Z.json` / `.log` | corrected model pass; flawed model `NoPartial` counterexample |
| `verification/tla/WeftCommitBuggy.counterexample.txt` | TLC trace quoted from the committed log |
| `bench/receipts/powerloss-20261103T224003Z.json` / `.log` | 30/30 VM power-cut iterations pass, 26 old + 4 new, 0 partial, 0 durability failures, 1 accepted unpublished tail, unsynced-control loss 30/30 |
| `bench/receipts/powerloss-20261103T224205Z.json` / `.log` | secondary writeback run: 10/10 pass, 0 partial, 2 accepted unpublished tails |
| `tests/bounded_recovery.rs` | test-only bounded corruption enumeration (not model checking) |
| `bench/receipts/crash-torture-*.json` | process-fault crash proxy, explicitly not physical power loss |
