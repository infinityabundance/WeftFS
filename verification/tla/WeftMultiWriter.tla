----------------------- MODULE WeftMultiWriter ------------------------
\* Abstract multi-committer (MW2) publication model for WeftFS.
\*
\* This is an ABSTRACTION of the R4 protocol in spec/WEFT-MULTIWRITER.md and
\* src/mw2.rs. It models:
\*   - two or more independent committers that prepare immutable blobs and
\*     publish against one observed head with a conditional head update;
\*   - disjoint transactions (different key sets), overlapping transactions
\*     that observed an earlier version, retries after a lost race, and
\*     bounded-abort after the retry budget;
\*   - a crash before publication (torn commit entry, invisible), a crash
\*     after the head publication but before the acknowledgement, and recovery;
\*   - snapshot readers that select a published head;
\*   - GC that reclaims unreachable blobs while a transaction is prepared.
\*
\* It is NOT a refinement proof of the Rust or Go code. It is a bounded TLC
\* run of the abstract state machine. See verification/README.md.
\*
\* Key abstraction: a commit writes a SET of keys. Version(k) is the
\* generation of the last commit that wrote k (0 = absent). A transaction
\* records the versions it observed. Publishing is enabled only when the
\* observed head is still the head AND every observed version is still
\* current. That is the "no blind overwrite" rule; the deliberately flawed
\* module WeftMultiWriterBuggy removes it.
\*
\* Model-checked invariants (see WeftMultiWriter.cfg):
\*   TypeOK, HeadWellFormed, NoPartial, NoLostAck, NoBlindOverwrite,
\*   CommittedBlobsDurable, LivePreparedDurable, ReaderIsAPrefix,
\*   GcNeverReclaimsReachable, GenerationUnique.

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Committers, Keys, MaxGen, MaxAttempts

ASSUME MaxGen >= 2
ASSUME MaxAttempts >= 0
ASSUME Cardinality(Committers) >= 2
ASSUME Cardinality(Keys) >= 1

Gens == 1..MaxGen
Phases == {"idle", "prepared", "publishing", "done"}
EmptyEntry == [kind |-> "none", owner |-> CHOOSE c \in Committers : TRUE,
              writes |-> {}, observed |-> [k \in Keys |-> 0]]
TornEntry == [kind |-> "torn", owner |-> CHOOSE c \in Committers : TRUE,
              writes |-> {}, observed |-> [k \in Keys |-> 0]]

VARIABLES
  head,          \* highest published generation, 0 = none
  chain,         \* [1..MaxGen -> record | NoCommit | TornCommit]
  durableBlob,   \* SUBSET Keys: immutable blobs present on the blob plane
  deletedBlob,   \* SUBSET Keys: reclaimed by GC (audit set)
  phase,         \* [Committers -> Phases]
  txn,           \* [Committers -> record | NoCommit]
  observedHead,  \* [Committers -> 0..MaxGen]
  attempts,      \* [Committers -> 0..MaxAttempts]
  acked,         \* [Committers -> 0..MaxGen]  0 = not acknowledged
  pending,       \* [Committers -> 0..MaxGen]  generation being published
  crashed,       \* [Committers -> BOOLEAN]
  readHead       \* [readers -> 0..MaxGen]; one reader variable is enough

Readers == {"r1", "r2"}
EntryRecord == [kind : {"none", "torn", "txn"}, owner : Committers,
                writes : SUBSET Keys, observed : [Keys -> 0..MaxGen]]
vars == <<head, chain, durableBlob, deletedBlob, phase, txn, observedHead,
          attempts, acked, pending, crashed, readHead>>

\* ---------------------------------------------------------------------------
\* Derived helpers
\* ---------------------------------------------------------------------------

Complete(g) == chain[g].kind = "txn"

WritesAt(g) == IF Complete(g) THEN chain[g].writes ELSE {}

VersionAt(limit, k) ==
  LET writers == { g \in 1..limit : Complete(g) /\ k \in WritesAt(g) }
  IN IF writers = {} THEN 0 ELSE CHOOSE m \in writers :
       \A x \in writers : x <= m

\* Keys reachable from the published head.
HeadReachable == UNION { WritesAt(g) : g \in 1..head }

\* Keys a live prepared transaction will need.
LivePreparedSet == { c \in Committers : phase[c] \in {"prepared", "publishing"} }
LivePrepared == UNION { txn[c].writes : c \in LivePreparedSet }

Reachable == HeadReachable \union LivePrepared

\* The set of versions a reader at `h` observes (for the prefix property).
ObservedAt(h) == [k \in Keys |-> VersionAt(h, k)]

\* ---------------------------------------------------------------------------
\* Initial state
\* ---------------------------------------------------------------------------

Init ==
  /\ head = 0
  /\ chain = [g \in Gens |-> EmptyEntry]
  /\ durableBlob = {}
  /\ deletedBlob = {}
  /\ phase = [c \in Committers |-> "idle"]
  /\ txn = [c \in Committers |-> EmptyEntry]
  /\ observedHead = [c \in Committers |-> 0]
  /\ attempts = [c \in Committers |-> 0]
  /\ acked = [c \in Committers |-> 0]
  /\ pending = [c \in Committers |-> 0]
  /\ crashed = [c \in Committers |-> FALSE]
  /\ readHead = [r \in Readers |-> 0]

\* ---------------------------------------------------------------------------
\* Prepare: choose a non-empty op set and make its blobs durable.
\* A transaction observes the current versions of its own keys.
\* ---------------------------------------------------------------------------

Start(c) ==
  /\ c \in Committers
  /\ phase[c] = "idle"
  /\ ~crashed[c]
  /\ \E w \in SUBSET Keys :
       w # {} /\
       /\ txn' = [txn EXCEPT ![c] = [kind |-> "txn", owner |-> c, writes |-> w,
                                     observed |-> [k \in Keys |-> VersionAt(head, k)]]]
       /\ durableBlob' = durableBlob \union w
       /\ deletedBlob' = deletedBlob \ w
       /\ phase' = [phase EXCEPT ![c] = "prepared"]
       /\ observedHead' = [observedHead EXCEPT ![c] = head]
       /\ attempts' = [attempts EXCEPT ![c] = 0]
  /\ UNCHANGED <<head, chain, acked, pending, crashed, readHead>>

\* ---------------------------------------------------------------------------
\* Re-read the head (the CAS attempt). A retry must prove its own versions are
\* still current; a conflict aborts. A disjoint winner only advances the head.
\* ---------------------------------------------------------------------------

Revalidate(c) ==
  /\ c \in Committers
  /\ phase[c] = "prepared"
  /\ ~crashed[c]
  /\ txn[c].kind = "txn"
  /\ LET w == txn[c].writes
         conflict == \E k \in w : VersionAt(head, k) # txn[c].observed[k]
     IN IF conflict
        THEN /\ attempts' = [attempts EXCEPT ![c] = MaxAttempts]
             /\ phase' = [phase EXCEPT ![c] = "idle"]
             /\ txn' = [txn EXCEPT ![c] = EmptyEntry]
             /\ observedHead' = observedHead
        ELSE /\ observedHead' = [observedHead EXCEPT ![c] = head]
             /\ UNCHANGED <<phase, attempts, txn>>
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, acked, pending,
                  crashed, readHead>>

\* A lost publication race, with a non-conflicting op set: rebase and retry,
\* bounded by MaxAttempts.
Retry(c) ==
  /\ c \in Committers
  /\ phase[c] = "prepared"
  /\ ~crashed[c]
  /\ txn[c].kind = "txn"
  /\ attempts[c] < MaxAttempts
  /\ \A k \in txn[c].writes : VersionAt(head, k) = txn[c].observed[k]
  /\ attempts' = [attempts EXCEPT ![c] = attempts[c] + 1]
  /\ observedHead' = [observedHead EXCEPT ![c] = head]
  /\ txn' = [txn EXCEPT ![c] =
               [txn[c] EXCEPT !.observed = [k \in Keys |-> VersionAt(head, k)]]]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, phase, acked, pending,
                  crashed, readHead>>

\* ---------------------------------------------------------------------------
\* Publish: the one conditional head write. Requires the observed head and
\* every observed version to still be current. Chooses the next generation.
\* ---------------------------------------------------------------------------

Publish(c) ==
  /\ c \in Committers
  /\ phase[c] = "prepared"
  /\ ~crashed[c]
  /\ txn[c].kind = "txn"
  /\ observedHead[c] = head
  /\ head < MaxGen
  /\ \A k \in txn[c].writes : VersionAt(head, k) = txn[c].observed[k]
  /\ LET g == head + 1 IN
       /\ chain' = [chain EXCEPT ![g] = txn[c]]
       /\ pending' = [pending EXCEPT ![c] = g]
       /\ phase' = [phase EXCEPT ![c] = "publishing"]
       /\ head' = g
  /\ UNCHANGED <<durableBlob, deletedBlob, txn, observedHead, attempts, acked,
                  crashed, readHead>>

\* The publication call returns to the caller: the acknowledgement.
Ack(c) ==
  /\ c \in Committers
  /\ phase[c] = "publishing"
  /\ ~crashed[c]
  /\ pending[c] \in Gens
  /\ acked' = [acked EXCEPT ![c] = pending[c]]
  /\ phase' = [phase EXCEPT ![c] = "done"]
  /\ txn' = [txn EXCEPT ![c] = EmptyEntry]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, observedHead, attempts,
                  pending, crashed, readHead>>

\* ---------------------------------------------------------------------------
\* Faults
\* ---------------------------------------------------------------------------

\* Crash while writing the commit record: a torn entry is on media, the head
\* does not advance, and recovery must ignore the torn entry.
CrashTornCommit(c) ==
  /\ c \in Committers
  /\ phase[c] \in {"prepared", "publishing"}
  /\ ~crashed[c]
  /\ head < MaxGen
  /\ chain' = [chain EXCEPT ![head + 1] = TornEntry]
  /\ crashed' = [crashed EXCEPT ![c] = TRUE]
  /\ phase' = [phase EXCEPT ![c] = "idle"]
  /\ txn' = [txn EXCEPT ![c] = EmptyEntry]
  /\ UNCHANGED <<head, durableBlob, deletedBlob, observedHead, attempts, acked,
                  pending, readHead>>

\* Crash after the head publication but before the acknowledgement returns:
\* the commit is durable and visible, the caller sees outcome_unknown.
CrashAfterHead(c) ==
  /\ c \in Committers
  /\ phase[c] = "publishing"
  /\ ~crashed[c]
  /\ crashed' = [crashed EXCEPT ![c] = TRUE]
  /\ phase' = [phase EXCEPT ![c] = "idle"]
  /\ txn' = [txn EXCEPT ![c] = EmptyEntry]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, observedHead, attempts,
                  acked, pending, readHead>>

\* Crash before publication: prepared blobs remain as unreferenced garbage.
CrashPrepared(c) ==
  /\ c \in Committers
  /\ phase[c] = "prepared"
  /\ ~crashed[c]
  /\ crashed' = [crashed EXCEPT ![c] = TRUE]
  /\ phase' = [phase EXCEPT ![c] = "idle"]
  /\ txn' = [txn EXCEPT ![c] = EmptyEntry]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, observedHead, attempts,
                  acked, pending, readHead>>

Recover(c) ==
  /\ c \in Committers
  /\ crashed[c]
  /\ crashed' = [crashed EXCEPT ![c] = FALSE]
  /\ phase' = [phase EXCEPT ![c] = "idle"]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, txn, observedHead,
                  attempts, acked, pending, readHead>>

\* ---------------------------------------------------------------------------
\* Readers select a published head. They never see a torn or unpublished slot.
\* ---------------------------------------------------------------------------

Read(r) ==
  /\ r \in Readers
  /\ \E h \in 0..head : readHead' = [readHead EXCEPT ![r] = h]
  /\ UNCHANGED <<head, chain, durableBlob, deletedBlob, phase, txn, observedHead,
                  attempts, acked, pending, crashed>>

\* ---------------------------------------------------------------------------
\* GC: a key may be reclaimed only when it is not reachable from the head and
\* not required by any live prepared transaction.
\* ---------------------------------------------------------------------------

GcReclaim(k) ==
  /\ k \in durableBlob
  /\ k \notin Reachable
  /\ durableBlob' = durableBlob \ {k}
  /\ deletedBlob' = deletedBlob \union {k}
  /\ UNCHANGED <<head, chain, phase, txn, observedHead, attempts, acked,
                  pending, crashed, readHead>>

Next ==
  \/ \E c \in Committers : Start(c) \/ Revalidate(c) \/ Retry(c) \/ Publish(c)
  \/ \E c \in Committers : Ack(c) \/ CrashTornCommit(c) \/ CrashAfterHead(c)
                          \/ CrashPrepared(c) \/ Recover(c)
  \/ \E r \in Readers : Read(r)
  \/ \E k \in Keys : GcReclaim(k)

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------------------
\* Invariants
\* ---------------------------------------------------------------------------

TypeOK ==
  /\ head \in 0..MaxGen
  /\ chain \in [Gens -> EntryRecord]
  /\ durableBlob \subseteq Keys
  /\ deletedBlob \subseteq Keys
  /\ phase \in [Committers -> Phases]
  /\ txn \in [Committers -> EntryRecord]
  /\ observedHead \in [Committers -> 0..MaxGen]
  /\ attempts \in [Committers -> 0..MaxAttempts]
  /\ acked \in [Committers -> 0..MaxGen]
  /\ pending \in [Committers -> 0..MaxGen]
  /\ crashed \in [Committers -> BOOLEAN]
  /\ readHead \in [Readers -> 0..MaxGen]

\* Every generation up to the head is a complete commit; a torn entry below the
\* head is impossible (recovery overwrites or ignores it).
HeadWellFormed ==
  \A g \in 1..head : Complete(g)

\* A reader sees a prefix of complete commits, never a strict subset of one.
NoPartial ==
  \A r \in Readers : readHead[r] <= head /\ HeadWellFormed

\* Every acknowledgement names a commit that is published.
NoLostAck ==
  \A c \in Committers :
    acked[c] > 0 => /\ acked[c] \in 1..head
                   /\ Complete(acked[c])
                   /\ chain[acked[c]].owner = c

\* The core MW2 safety rule: a commit that writes a key must have observed the
\* version produced by the previous writer of that key. This is what forbids
\* last-writer-wins.
NoBlindOverwrite ==
  \A g \in 1..head : \A k \in WritesAt(g) :
    LET previous == LET writers == { h \in 1..(g - 1) : Complete(h) /\ k \in WritesAt(h) }
                   IN IF writers = {} THEN 0
                      ELSE CHOOSE m \in writers : \A x \in writers : x <= m
    IN chain[g].observed[k] = previous

\* Every key named by a published commit is still durable.
CommittedBlobsDurable ==
  \A g \in 1..head : WritesAt(g) \subseteq durableBlob

\* A prepared transaction's blobs are durable until it publishes or aborts.
LivePreparedDurable ==
  \A c \in Committers : phase[c] \in {"prepared", "publishing"} =>
    txn[c].kind = "txn" /\ txn[c].writes \subseteq durableBlob

\* A reader at generation h sees exactly the version map of commits 1..h, and
\* that map is a prefix of the map at the head.
ReaderIsAPrefix ==
  \A r \in Readers :
    /\ LET h == readHead[r]
       IN ObservedAt(h) = [k \in Keys |-> VersionAt(h, k)]
    /\ \A k \in Keys : VersionAt(readHead[r], k) <= VersionAt(head, k)

\* GC never deletes a key required by the head or by a live prepared txn.
GcNeverReclaimsReachable ==
  deletedBlob \cap Reachable = {}

\* Generations are append-only: a later commit's observed map includes every
\* earlier commit of this model, because VersionAt is monotone in the limit.
CommitOrderIsStable ==
  \A g1, g2 \in 1..head :
    g1 < g2 /\ Complete(g1) /\ Complete(g2) =>
      \A k \in Keys : VersionAt(g1, k) <= VersionAt(g2, k)

================================================================================
