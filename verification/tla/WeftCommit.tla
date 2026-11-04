------------------------------- MODULE WeftCommit -------------------------------
\* Abstract commit/recovery state machine for WeftFS v1.
\*
\* This is an ABSTRACTION, not the implementation. The mapping to
\* src/store.rs (publish_commit), src/store.rs (store_blob),
\* src/recovery.rs (recover/load_state_at), and src/format.rs
\* (superblock slot encoding/CRC) is documented in verification/README.md.
\*
\* Modelled crash points:
\*   - after object writes but before their fsync: no commit file, old state
\*   - during the commit-file write: a torn commit file (CRC fails)
\*   - after the commit-file fsync, before the superblock write: old slot
\*   - during the superblock slot write: a torn slot (CRC fails)
\*   - after the slot fsync, before the commit call returns: new state visible
\*   - after the call returns: new state visible and the caller was told so
\*
\* Assumptions (see README): fsync is honoured; a synced write is complete and
\* durable; an unsynced write may be lost or torn; object blobs are atomic
\* because they are fsynced under tmp/ and renamed; commit files are immutable;
\* there is one writer and no GC. Checkpoints are NOT modelled.
\*
\* Constants: MaxGen generations, two operations per commit, two superblock
\* slots alternating by generation. The two operations make a partial commit
\* observable as a state that is neither the old nor the new whole state.
\*
\* Model-checked (TLC over the bounded state space) invariants:
\*   TypeOK, NoPartial, HighestValid, NoLostDependency, Durability,
\*   ReturnedObjectsDurable, ReplayIdempotent.
\* No claim is made that the Rust or Go implementation is verified.

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS MaxGen

ASSUME MaxGen >= 2

Gens    == 1..MaxGen
Slots   == 0..1
Objects == 1..(2 * MaxGen)
AllOps  == {<<g, k>> : g \in Gens, k \in 1..2}
Phases  == {"idle", "objects", "objectsSynced", "commitWritten",
            "commitSynced", "slotWritten", "slotSynced"}
CommitStates == {"none", "torn", "valid"}
EmptySlot    == 0
TornSlot     == MaxGen + 1
SlotValues   == 0..(MaxGen + 1)   \* 0 empty, 1..MaxGen generation, MaxGen+1 torn

Ops(g)         == {<<g, 1>>, <<g, 2>>}
RequiredObj(g) == {g, g + MaxGen}
SlotOf(g)      == g % 2

VARIABLES
  commit,          \* [Gens -> CommitStates] commit-file state on media
  durableObj,      \* SUBSET Objects: objects present and fsynced
  slot,            \* [0..1 -> SlotValues] superblock slot contents
  phase,           \* commit phase, or "idle"
  pending,         \* generation being committed, 0 when idle
  returned,        \* SUBSET Gens: commit calls that returned success
  crashed,         \* a power cut happened
  scan,            \* recovery scan cursor (MaxGen .. 0)
  recovered,       \* selected generation, 0 = none
  applyGen,        \* replay cursor
  applied,         \* SUBSET AllOps: replayed op set
  recoveryDone,    \* scan and replay finished
  recoveredAgain,  \* second recovery pass result
  appliedAgain,    \* second recovery pass op set
  againDone        \* second recovery pass finished

vars == <<commit, durableObj, slot, phase, pending, returned, crashed,
          scan, recovered, applyGen, applied, recoveryDone,
          recoveredAgain, appliedAgain, againDone>>

Init ==
  /\ commit = [g \in Gens |-> "none"]
  /\ durableObj = {}
  /\ slot = [s \in Slots |-> EmptySlot]
  /\ phase = "idle"
  /\ pending = 0
  /\ returned = {}
  /\ crashed = FALSE
  /\ scan = MaxGen
  /\ recovered = 0
  /\ applyGen = 1
  /\ applied = {}
  /\ recoveryDone = FALSE
  /\ recoveredAgain = 0
  /\ appliedAgain = {}
  /\ againDone = FALSE

\* ---------------------------------------------------------------------------
\* Commit path. Every write action is ordered; a crash may interrupt between any
\* two actions, and the two "Crash...Write" actions model a torn in-place write.
\* ---------------------------------------------------------------------------

Start(g) ==
  /\ g \in Gens
  /\ ~crashed
  /\ phase = "idle"
  /\ pending = 0
  /\ returned = 1..(g - 1)
  /\ pending' = g
  /\ phase' = "objects"
  /\ UNCHANGED <<commit, durableObj, slot, returned, crashed, scan, recovered,
                  applyGen, applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

SyncObjects ==
  /\ phase = "objects"
  /\ pending \in Gens
  /\ durableObj' = durableObj \union RequiredObj(pending)
  /\ phase' = "objectsSynced"
  /\ UNCHANGED <<commit, slot, pending, returned, crashed, scan, recovered,
                  applyGen, applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

WriteCommit ==
  /\ phase = "objectsSynced"
  /\ phase' = "commitWritten"
  /\ UNCHANGED <<commit, durableObj, slot, pending, returned, crashed, scan,
                  recovered, applyGen, applied, recoveryDone, recoveredAgain,
                  appliedAgain, againDone>>

\* Crash while the commit file is being written: a strict prefix is on media.
\* CRC validation rejects it; the buggy model ignores that validation.
CrashCommitWrite ==
  /\ phase = "commitWritten"
  /\ pending \in Gens
  /\ commit' = [commit EXCEPT ![pending] = "torn"]
  /\ crashed' = TRUE
  /\ phase' = "idle"
  /\ pending' = 0
  /\ UNCHANGED <<durableObj, slot, returned, scan, recovered, applyGen, applied,
                  recoveryDone, recoveredAgain, appliedAgain, againDone>>

SyncCommit ==
  /\ phase = "commitWritten"
  /\ pending \in Gens
  /\ commit' = [commit EXCEPT ![pending] = "valid"]
  /\ phase' = "commitSynced"
  /\ UNCHANGED <<durableObj, slot, pending, returned, crashed, scan, recovered,
                  applyGen, applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

WriteSlot ==
  /\ phase = "commitSynced"
  /\ phase' = "slotWritten"
  /\ UNCHANGED <<commit, durableObj, slot, pending, returned, crashed, scan,
                  recovered, applyGen, applied, recoveryDone, recoveredAgain,
                  appliedAgain, againDone>>

\* Crash while the in-place superblock slot is being written: CRC fails.
CrashSlotWrite ==
  /\ phase = "slotWritten"
  /\ pending \in Gens
  /\ slot' = [slot EXCEPT ![SlotOf(pending)] = TornSlot]
  /\ crashed' = TRUE
  /\ phase' = "idle"
  /\ pending' = 0
  /\ UNCHANGED <<commit, durableObj, returned, scan, recovered, applyGen,
                  applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

SyncSlot ==
  /\ phase = "slotWritten"
  /\ pending \in Gens
  /\ slot' = [slot EXCEPT ![SlotOf(pending)] = pending]
  /\ phase' = "slotSynced"
  /\ UNCHANGED <<commit, durableObj, pending, returned, crashed, scan, recovered,
                  applyGen, applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

\* The commit call returns only after the slot fsync and the root-dir fsync.
Return ==
  /\ phase = "slotSynced"
  /\ pending \in Gens
  /\ returned' = returned \union {pending}
  /\ phase' = "idle"
  /\ pending' = 0
  /\ UNCHANGED <<commit, durableObj, slot, crashed, scan, recovered, applyGen,
                  applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

\* A power cut that does not tear the write in flight. The media is unchanged.
Crash ==
  /\ ~crashed
  /\ crashed' = TRUE
  /\ phase' = "idle"
  /\ pending' = 0
  /\ UNCHANGED <<commit, durableObj, slot, returned, scan, recovered, applyGen,
                  applied, recoveryDone, recoveredAgain, appliedAgain,
                  againDone>>

\* ---------------------------------------------------------------------------
\* Recovery: scan from MaxGen down, accept the first candidate, then replay
\* commits 1..recovered. Candidate is the abstraction of every check in
\* recovery::recover (valid slot bound, commit CRC/hash, complete chain,
\* referenced object existence).
\* ---------------------------------------------------------------------------

MaxOf(S) == CHOOSE m \in S : \A x \in S : x <= m

MaxVisible ==
  LET visible == {slot[s] : s \in Slots} \cap Gens
  IN IF visible = {} THEN 0 ELSE MaxOf(visible)

Candidate(g) ==
  /\ g \in Gens
  /\ commit[g] = "valid"
  /\ g <= MaxVisible
  /\ \A i \in 1..g : commit[i] = "valid"
  /\ \A i \in 1..g : RequiredObj(i) \subseteq durableObj

CommitOps(g) ==
  IF commit[g] = "valid" THEN Ops(g)
  ELSE IF commit[g] = "torn" THEN {<<g, 1>>}
  ELSE {}

StateOf(g) == UNION {Ops(i) : i \in 1..g}

RecoverStep ==
  /\ crashed
  /\ ~recoveryDone
  /\ scan > 0
  /\ \/ /\ Candidate(scan)
        /\ recovered' = scan
        /\ scan' = 0
        /\ applyGen' = 1
        /\ applied' = {}
        /\ recoveryDone' = FALSE
     \/ /\ ~Candidate(scan)
        /\ scan' = scan - 1
        /\ recoveryDone' = (scan = 1)
        /\ UNCHANGED <<recovered, applyGen, applied>>
  /\ UNCHANGED <<commit, durableObj, slot, phase, pending, returned, crashed,
                  recoveredAgain, appliedAgain, againDone>>

ReplayStep ==
  /\ crashed
  /\ ~recoveryDone
  /\ scan = 0
  /\ recovered > 0
  /\ applyGen <= recovered
  /\ applied' = applied \union CommitOps(applyGen)
  /\ applyGen' = applyGen + 1
  /\ recoveryDone' = (applyGen = recovered)
  /\ UNCHANGED <<commit, durableObj, slot, phase, pending, returned, crashed,
                  scan, recovered, recoveredAgain, appliedAgain, againDone>>

\* A second recovery pass over the same immutable media must yield the same state.
RecoverAgain ==
  /\ recoveryDone
  /\ ~againDone
  /\ recoveredAgain' = recovered
  /\ appliedAgain' = UNION {CommitOps(i) : i \in 1..recovered}
  /\ againDone' = TRUE
  /\ UNCHANGED <<commit, durableObj, slot, phase, pending, returned, crashed,
                  scan, recovered, applyGen, applied, recoveryDone>>

Next ==
  \/ \E g \in Gens : Start(g)
  \/ SyncObjects \/ WriteCommit \/ CrashCommitWrite \/ SyncCommit
  \/ WriteSlot \/ CrashSlotWrite \/ SyncSlot \/ Return
  \/ Crash
  \/ RecoverStep \/ ReplayStep \/ RecoverAgain

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------------------
\* Invariants
\* ---------------------------------------------------------------------------

TypeOK ==
  /\ commit \in [Gens -> CommitStates]
  /\ durableObj \subseteq Objects
  /\ slot \in [Slots -> SlotValues]
  /\ phase \in Phases
  /\ pending \in 0..MaxGen
  /\ returned \subseteq Gens
  /\ crashed \in BOOLEAN
  /\ scan \in 0..MaxGen
  /\ recovered \in 0..MaxGen
  /\ applyGen \in 1..(MaxGen + 1)
  /\ applied \subseteq AllOps
  /\ recoveryDone \in BOOLEAN
  /\ recoveredAgain \in 0..MaxGen
  /\ appliedAgain \subseteq AllOps
  /\ againDone \in BOOLEAN

\* I5 atomicity: recovery never exposes a commit prefix or a mixed state.
NoPartial ==
  recoveryDone => applied = StateOf(recovered)

\* Recovery selects the highest generation that passes every validity check.
HighestValid ==
  recoveryDone =>
    /\ (recovered = 0 \/ Candidate(recovered))
    /\ \A g \in Gens : g > recovered => ~Candidate(g)

\* I6 reference integrity: every generation replayed has all of its objects.
NoLostDependency ==
  recoveryDone => \A i \in 1..recovered : RequiredObj(i) \subseteq durableObj

\* A commit call that returned survives a subsequent power cut.
Durability ==
  (crashed /\ recoveryDone) => \A g \in returned : recovered >= g

\* Objects of a returned commit were durable before the commit became visible.
ReturnedObjectsDurable ==
  \A g \in returned : RequiredObj(g) \subseteq durableObj

\* Replay is idempotent over the immutable media.
ReplayIdempotent ==
  againDone => appliedAgain = applied

================================================================================
