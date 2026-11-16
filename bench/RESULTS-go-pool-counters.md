# X21 — Go pooling: separate counters and a restoration result

Receipt: `bench/receipts/go-pool-counters-20261116T000103Z.json` (stamp
`20261116T000103Z`, WeftFS revision `4ab9ed2`, WeftFS-Go revision `ad293d2`). Raw
log beside it. Go unit test:
[`WeftFS-Go/store/blob_counters_test.go`](../WeftFS-Go/store/blob_counters_test.go).

## 1. What was built

The Go store counted nothing, so a receipt could not say whether a read was cheap
because the pool served it or because the workload was small. Two families are
now separate and named for what they measure:

* **`total_blob_reads` / `total_blob_bytes`** — the *logical* verified blob reads
  the store performs for its callers, one per `ReadObject`/`ReadContent`. This is
  what a workload asks for.
* **`backend_reads` / `backend_bytes`** — the *physical* reads the flat blob plane
  issued, one per `Get`/`GetRange`, whether a pooled descriptor served it or an
  open/read/close did. This is what the storage saw.

The pool's own counters (`pool_reuses`, `pool_refreshes`, `opens`, `evictions`)
are reported alongside, which is what turns "10,000 logical reads" into "10,000
logical reads, 9,936 descriptor reuses, 64 opens".

Failed reads never move the total counters. `ResetBlobCounters` zeroes a window;
the pool's lifetime counters are not reset. None of this is on the on-disk
format, and no store decision reads a counter.

## 2. Method

Harness `examples/pool_restore_bench.rs` plus `WeftFS-Go/cmd/poolbench`, court
`scripts/go_pool_court.sh`, fuse court. One store **written once by Rust** and
read by both languages, so the bytes, the blob layout and the chunking are
identical; only the reader differs. Four access patterns plus a failure arm:

| arm | workload |
|---|---|
| `flat` | 64 objects, every object read once per round, seeded random order, 3 rounds |
| `hot` | 4 objects, 100 rounds (the working set is inside the pool's idle bound) |
| `hot1` | 1 object, 200 rounds |
| `packed` | the same 64-object workload over a packed store |
| `vanished` | 64 objects, most chunks removed: the read must fail closed |

The fixture is synthetic BF16 with a 4 KiB chunk size so each object has many
chunks. Counters are read after the work, in both languages, from the same
process that did it.

## 3. Results

<!-- BEGIN GENERATED:go-pool-table (scripts/perf_numbers.py) -->
| arm | note | Rust logical | Go logical | Rust backend | Go backend | Rust wall ms | Go wall ms |
|---|---|---:|---:|---:|---:|---:|---:|
| flat | 64 objects, 3 rounds | 192 | 192 | 6,336 | 6,336 | 38.3 | 83.8 |
| hot | 4 objects, 100 rounds | 400 | 400 | 13,200 | 13,200 | 78.0 | 171.5 |
| hot1 | 1 object, 200 rounds | 200 | 200 | 6,600 | 6,600 | 38.3 | 74.0 |
| packed | 64 objects over a packed store | 192 | 192 | 12,929 | 6,336 | 60.4 | 122.1 |
| vanished | chunks removed: both must refuse | 64 | 17 | 561 | 561 | 3.5 | 7.1 |
<!-- END GENERATED:go-pool-table -->

<!-- BEGIN GENERATED:go-pool-prose (scripts/perf_numbers.py) -->
The court ran 11 of 11 checks and all passed. On the flat arm both languages issue 192 logical reads over 25,165,824 B and 6,336 backend reads over 25,510,848 B, and Go takes 2.18× Rust's wall. The same relation holds for hot (2.20×), one object (1.93×) and packed (2.02×).
In the one-object arm Go's pool reuses a descriptor 6,567 times over 6,600 plane reads with 97 opens. On the packed store Rust records 12,929 backend reads against Go's 6,336 for identical logical reads. On the vanished arm both refuse 47 of 64 reads with `NotFound: no such object` and issue 561 backend reads; their logical totals differ by definition (64 harness calls versus 17 completed reads).
<!-- END GENERATED:go-pool-prose -->

## 4. Losses

1. **Go is about twice as slow as Rust on this read path**, in every arm, with
   identical logical and physical counters. The ratio is reported per arm in the
   generated paragraph. The counters explain the cost: the same number of plane
   reads, the same bytes, and the same descriptor reuse pattern — the difference
   is the Go read path's own work, not extra I/O.
2. **The packed arms disagree about physical reads.** Rust counts more backend
   reads than Go on the packed store while the logical counters are identical;
   Rust's read path fetches per-range where the Go plane serves a pack member in
   one read. Report both, do not average them.
3. **The vanished arm's logical totals differ by definition.** Rust's
   `total_blob_reads` is the harness's `read_object` call count (64), while Go's
   counts completed logical reads (17 of 64). The backend counters agree exactly
   (561), and both languages refuse the same 47 reads with the same
   `NotFound: no such object`. This is a counter-definition difference, not a
   behavioural one; it is published because a reader comparing the two totals
   would otherwise see a discrepancy.
4. **The pool's reuse counter is not a path-match guarantee in Rust.** Rust's
   `handles_reused` counts pooled-handle re-acquisitions; the Go pool counts a
   reuse only when the idle descriptor is for the same path. The two numbers are
   therefore not directly comparable and are not compared.
5. **`hot` is not hot in the Rust sense.** The 4-object working set is inside the
   64-handle idle bound, so Go reuses descriptors and still pays its own per-read
   cost; the arm measures the pool, not the page cache.
6. **This is a read-path court, not a checkpoint restore.** It reads objects
   through the object API; it says nothing about `restore_checkpoint` throughput.
7. **One host, warm page cache, synthetic BF16.** No fleet, cloud or media claim;
   no Go change to the on-disk format.

## 5. Non-claims

* No claim that Go pooling is faster than Rust, or than not pooling: the
  comparison here is Go versus Rust on one store, and the pool counters are
  descriptive.
* No Go S3/GCS/Azure claim: the court is the local flat plane (packed and
  un-packed).
* No change to any frozen byte, mode number or root; the counters are
  diagnostics.
