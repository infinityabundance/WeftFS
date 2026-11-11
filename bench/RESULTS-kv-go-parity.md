# Store-level KV parity, Rust and Go (R14c / R15)

**Status:** implemented, court-tested in the Docker courts.
**Revisions:** parent `08456c1` (Rust verb + Rust test + spec), WeftFS-Go
`225eb0d` (Go handoff test + benchmarks + spec mirror), WeftCodec `67cea30`
and WeftCodec-Go `5e047c3` (spec mirrors only). Go store code landed earlier at
WeftFS-Go `db35a4e`.

This note covers two things the programme had claimed only partially:

1. the Go store now enforces the mode-12 and mode-19 KV capability surface that
   `src/kv_capability.rs` implements in Rust; and
2. the Rust→Go handoff is a **writer-to-reader** test. Rust no longer copies
   attribute values it printed from a Go-written object; the Rust CLI has its
   own page-level writer, and both languages must derive the same identity.

Neither change touches a wire format. Mode 12 and mode 19 frames are unchanged
and the frozen fixtures are untouched.

---

## 1. What exists in each language

| Surface | Rust (`src/kv_capability.rs`) | Go (`store/kv.go`) |
|---|---|---|
| write mode-12 page + capability descriptor | `Store::write_kv_page` | `Store.WriteKvPage` |
| write mode-19 page + provenance-bound descriptor | `Store::write_kv_prov_page` | `Store.WriteKvProvPage` |
| authorized read, one denial text | `read_kv_page` / `read_kv_prov_page` | `ReadKvPage` / `ReadKvProvPage` |
| descriptor types + strict parsers | `KvDescriptor` / `KvProvDescriptor` | `KvDescriptor` / `KvProvDescriptor` |
| reserved attributes (`weft.kv.*`) | `descriptor_attrs` / `prov_descriptor_attrs` | identical names and values |
| CLI page writer | `weftfs kv prov-write` (new here) | none |
| KV prefix tier (`kv_prefix_*`, `kv gc`) | `src/kv_prefix.rs` + `weftfs kv` | **absent** |
| capability envelope verification | Ed25519 in `src/auth.rs` | **absent** (caller passes `*auth.Capability`) |

The normative read order is unchanged in both languages: object record →
descriptor → derived descriptor id → capability presence/validity →
tenant/namespace/root grant → POSIX authorization → read. Every failure before
a byte is returned is the single text `kv page is not available to this
principal` with class `Unauthorized`; only post-authorization mismatches are
`Corrupt`. The mode-12 reader refuses a mode-19 page and vice versa.

Go's API difference is deliberate and documented in the spec: Rust's
`Principal` carries an already-verified envelope, Go has no envelope type
anywhere, so `KvPrincipal{Principal, Capability}` is the union and a nil
capability is denied exactly like an absent envelope.

---

## 2. The Rust verb

```text
weftfs --store DIR kv prov-write \
  --name LEAF --tenant T --model M --layer N --session S \
  --page-index I --page-count C --seq-start A --seq-end B \
  --dtype bf16 --heads H --head-dim D --payload ELEMENTS.bin \
  --model-root HEX64 [--adapter-root HEX64 ...] \
  --tokenizer-scheme SCHEME --vocab-size V \
  --sliding-window W --rope-theta T --rope-scale S --positional rope \
  --page-rows R --layer-count L --salt HEX64 [--uid U] [--gid G] [--json]
```

`--payload` is the raw little-endian element stream, not a frame. The verb
encodes it with the shipped mode-19 encoder, re-parses the frame, reads the
codec descriptor back out and uses **that** root as the provenance root — the
same rule as `Store::write_kv_prov_page`. It prints the object id, descriptor
id, provenance root, frame bytes and page bytes.

```text
$ weftfs --store /tmp/kvsmoke/store kv prov-write ... --json
{ "descriptor_id": "b2edc38b...d4df917f",
  "frame_bytes": 1443,
  "generation": 2,
  "mode": 19,
  "object": "9c6ae26f...9dffa8",
  "page_bytes": 4096,
  "provenance_root": "da423113...b53f1c",
  "tenant": "tenant-a" }

$ weftfs --store /tmp/kvsmoke/store cat /rewritten.kv > frame.bin
$ weftcodec decode --input frame.bin --output out.bin
$ cmp out.bin elements.bin && echo PAYLOAD_EXACT
PAYLOAD_EXACT
$ weftfs --store /tmp/kvsmoke/store verify --full
...
checked_capabilities=0
```

(In that smoke the element bytes were zeros, so the ids are not the court ids
below. It is a shape check, not the parity evidence.)

---

## 3. Cross-language parity (the actual claim)

Both implementations are given the same geometry, the same provenance inputs
and the same 4,096 element bytes (8 rows × 4 heads × 64 dims × BF16). Every
identity in this format is content-derived, so agreement is not a convention.

| quantity | Rust (CLI verb) | Go (`WriteKvProvPage`) | equal |
|---|---|---|---|
| descriptor id | `9c8344b07bcf0560af65d704201ae14c288edf3863393c43242d03d366ea3b1a` | same | yes |
| provenance root | `ba5ec110c7cb4cd79c330dc3103fd6e7fc4d2b9eaae3d0a949b826c90d5d7f5f` | same | yes |
| frame bytes | 5,509 | 5,509 | byte-identical |
| decoded page | 4,096 | 4,096 | byte-identical |

Court output:

```text
--- PASS: TestKvProvHandoffRustThenGo (0.01s)
    kv_test.go:1089: COURT rust_verb_written descriptor_id=9c8344b0...6ea3b1a
      root=ba5ec110...0d5d7f5f frame_bytes=5509 exact=true
--- PASS: TestKvProvHandoffGoThenRust (0.02s)
    kv_test.go:779: COURT rust_cat bytes=5509 equals_go_stored=true
    kv_test.go:798: COURT rust_descriptor_roots a=ba5ec110...0d5d7f5f b=ef7309ea...24618e distinct=true
    kv_test.go:820: COURT rust_codec_decoded bytes=4096 exact=true
```

The second test is the other direction: Go writes, Rust's CLI reads the frame
and parses the descriptor, and Rust's codec decodes it. Two provenance inputs
that differ only in an adapter root produce different roots.

The Rust half is `tests/kv_provenance.rs::cli_prov_write_matches_the_store_api`:
the CLI and `Store::write_kv_prov_page` must publish the same descriptor id,
provenance root and frame for identical inputs, and the stored frame must
decode to the exact element bytes.

---

## 4. Commands and results

All runs are in the Docker courts with the standard hard limits (3 GiB memory
and swap, 1,024 pids, 4 CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

### Rust

```sh
./scripts/docker-court.sh rust \
  "cd /work && cargo test --locked --test kv_provenance"
```

```text
running 6 tests
test cli_prov_write_matches_the_store_api ... ok
test collision_two_adapter_roots_are_two_identities ... ok
test adapter_set_is_canonical ... ok
test forgery_is_rejected ... ok
test salt_makes_identical_pages_unlinkable_by_root ... ok
test store_scope_is_the_provenance_descriptor_id ... ok
test result: ok. 6 passed; 0 failed; 0 ignored
```

Log: `bench/receipts/kv-go-parity-rust-20261111T154406Z.log`.

Full parent suite in the same court (the change is additive, but the gate is
the whole tree):

```sh
./scripts/docker-court.sh rust "cd /work && cargo test --locked --all-targets"
```

**158 suites, 772 passed, 0 failed, 0 ignored.** Log:
`bench/receipts/kv-go-parity-rust-suite-20261111T154406Z.log`.

### Go (focused, go court)

```sh
./scripts/docker-court.sh go \
  "cd /work/WeftFS-Go && gofmt -l ./store ./cmd && go vet ./... \
   && CGO_ENABLED=0 go build ./... \
   && go test -race -count=1 -run 'TestKv' ./store -v"
```

6/6 tests pass (log `bench/receipts/kv-go-parity-go-20261111T154406Z.log`):
`TestKvStoreScopeIsTheProvenanceDescriptorID`,
`TestKvProvForgeryIsRejected`, `TestKvDescriptorCanonicalForms`,
`TestKvProvHandoffGoThenRust`, `TestKvPageHandoffRustThenGo`,
`TestKvProvHandoffRustThenGo`.

### Go (full suite, fuse court)

```sh
./scripts/docker-court.sh fuse \
  "cd /work/WeftFS-Go && gofmt -l ./store ./cmd && go vet ./... \
   && CGO_ENABLED=0 go build ./... && go test -race -count=1 ./..."
```

```text
ok  github.com/infinityabundance/WeftFS-Go/cmd/weftfs   44.544s
ok  github.com/infinityabundance/WeftFS-Go/fusefs       7.273s
ok  github.com/infinityabundance/WeftFS-Go/index         1.004s
ok  github.com/infinityabundance/WeftFS-Go/model         1.009s
ok  github.com/infinityabundance/WeftFS-Go/store       215.102s
```

gofmt, `go vet` and the CGO-disabled build are clean. Log:
`bench/receipts/kv-go-parity-fuse-20261111T154406Z.log` (the run's environment
line names parent `08456c1`, WeftFS-Go `225eb0d`, WeftCodec `67cea30`,
WeftCodec-Go `5e047c3`).

### Go microbenchmarks (this is a Go-only number)

```sh
./scripts/docker-court.sh go \
  "cd /work/WeftFS-Go && go test -race -count=1 -run '^\$' \
   -bench 'BenchmarkWriteKvProvPage4KiB|BenchmarkReadKvProvPage4KiB|BenchmarkReadKvProvPage64KiB' \
   -benchtime=200x -benchmem ./store"
```

```text
BenchmarkWriteKvProvPage4KiB-4   200  10555622 ns/op  0.39 MB/s  4076811 B/op  9878 allocs/op
BenchmarkReadKvProvPage4KiB-4    200    696958 ns/op  5.88 MB/s   238061 B/op   531 allocs/op
BenchmarkReadKvProvPage64KiB-4   200   2457414 ns/op 26.67 MB/s   405611 B/op   596 allocs/op
```

Log: `bench/receipts/kv-go-bench-20261111T154406Z.log`.

Read the numbers as costs, not as a competitive result:

* **A write is a commit.** Each `WriteKvProvPage` opens, mutates and publishes
  its own generation. 10.56 ms and 4.08 MB allocated per 4 KiB page is the
  single-commit cost, not a bulk ingest rate; the measured group-commit path
  (1,000 creates in 22 ms) is the shape a real writer should use. This is the
  same class of loss published in `RESULTS-augmentation.md` and
  `RESULTS-gc-accounting.md`, now visible at page granularity.
* **A read allocates ~238 KB for a 4 KiB page** and ~406 KB for 64 KiB. The
  Go read path has no decoded-object cache and no single-flight, so the
  descriptor parse, capability checks and plane decode are all paid per call.
  The Rust store has `object_cache` + `singleflight`; the Go store does not.
  That is a concrete next increment (T065/P11), not a claim.
* These are 200-iteration runs on one in-court CPU, warm cache, tmpfs. No p50,
  p99, throughput, Rust-comparison or fleet figure follows from them.

---

## 5. Non-claims and remaining gaps

* No KV **prefix** tier in Go: `kv_prefix_put`, `kv_prefix_lookup`,
  `kv_prefix_get` and `kv gc` remain Rust-only. The prefix record and the
  mode-12 page are different surfaces.
* No envelope verification in the Go store. The Go denial depends on the
  caller passing an `*auth.Capability`; it is not a signature check.
* No FUSE-facing KV path in either language, and no serving-engine connector
  (vLLM/SGLang). Mode 19 remains a store-and-codec capability.
* No Go CLI verb for `kv prov-write`; the Go writer is the library API, which
  is what the tests and the store itself use.
* No encryption, confidentiality, timing or access-pattern privacy claim; the
  salt separates identifiers only (see `RESULTS-kv-provenance.md` §5).
* Single host, one container, tmpfs, warm cache. No media, power-loss, fleet
  or cloud claim.
* The Go benchmark is 200 iterations of a commit-bound path; it is not a
  distribution and must not be cited as one.

## 6. Reproduce

```sh
./scripts/docker-court.sh rust "cd /work && cargo test --locked --test kv_provenance"
./scripts/docker-court.sh go   "cd /work/WeftFS-Go && go test -race -count=1 -run 'TestKv' ./store -v"
./scripts/docker-court.sh fuse "cd /work/WeftFS-Go && go test -race -count=1 ./..."
./scripts/docker-court.sh go   "cd /work/WeftFS-Go && go test -run '^\$' -bench 'KvProvPage' -benchtime=200x -benchmem ./store"
```

The Go tests require the Rust oracle at
`.docker-cache/target-rust/release/weftfs` and skip (never pass) if it is
missing or unloadable. Build it in the rust court:

```sh
./scripts/docker-court.sh rust "cd /work && cargo build --release --locked --bin weftfs"
```
