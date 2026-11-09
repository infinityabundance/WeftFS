# Provenance-bound KV cache pages (R5)

**Status: implemented in both codecs; store/capability path Rust-only.** This
document records what was measured and every loss. The normative definition is
`spec/WEFT-KV-PROVENANCE.md`; the mode is `KV_CACHE_PROV` (mode 19).

## 1. The defect and the fix

Mode 12 `KV_CACHE` freezes a page's bytes and geometry. Provenance (model
weights, adapters, tokenizer, attention configuration, positional treatment,
layout, sharing domain) lived only in store metadata (`weft.kv.descriptor`,
`weft.kv.prefix.v1`). A consumer that resolves a page by content hash alone could
therefore reuse an incompatible prefix silently.

Mode 19 puts the provenance **inside the frame descriptor** and makes the page
identity a hash over it:

```text
descriptor_root = H("weft/kv/prov/v1", WCB(descriptor))
descriptor      = mode-12 page descriptor
                + model_root
                + adapter_roots (sorted, unique) + adapter_set_root
                + tokenizer {scheme, vocab_size} + tokenizer_root
                + attention {heads, head_dim, sliding_window, rope theta/scale bits}
                + positional + layout {dtype, page_rows, layer_count}
                + salt (32 bytes) + extensions (sorted, unique)
                + payload_hash
```

The payload is the mode-12 payload **verbatim**; the only wire difference is the
descriptor envelope. Mode 12 is byte-identical to before.

## 2. Adversarial court (raw outcomes)

Receipt: `bench/receipts/kv-provenance-court-20261109T182414Z.json`, raw log
beside it (`…Z.log`). Five tests, all pass; the numbers below are the exact
values printed by the court.

### 2.1 Collision: identical bytes, different adapter roots

| value | observed |
|---|---|
| payload bytes | 4096 |
| payload hash (both frames) | `a126fc3da63ffdf02eaa1d60d645b9da15dee5674388367f5d93e864347246c9` |
| root (adapter `0x21…`) | `ba5ec110c7cb4cd79c330dc3103fd6e7fc4d2b9eaae3d0a949b826c90d5d7f5f` |
| root (adapter `0x22…`) | `ef7309eabf7fdf2a25b0f9d37f0f6b87d301c3a8d22fd756332827cc2b24618e` |
| roots distinct | **true** |
| cross-resolve under the other's scope | **`Unauthorized`, 0 bytes emitted** |
| resolve under the matching scope | `ok`, 4096 bytes |

### 2.2 Sharing-domain salt

| value | observed |
|---|---|
| salt a / salt b | `b0b0…b0` / `b1b1…b1` |
| root a / root b | `81700d3d…f3483a` / `48b0c60e…6254a5` |
| payload hash equal | true |
| roots distinct | **true** |
| proven | the roots differ |
| **not** proven | timing, size, access-pattern, or existence privacy |

### 2.3 Adapter-set ordering

`{A,B}` and `{B,A}` produce one frame (`ab_equals_ba=true`); `{A}` differs
(`ab_equals_a=false`). Set roots:
`{A,B}` = `aad45143…12c59a`, `{A}` = `7cfe9a86…ad7bdf`.

### 2.4 Forgery

| forgery | result |
|---|---|
| tampered `adapter_set_root` with a recomputed digest | `Malformed` (derived-root check) |
| tampered `tokenizer_root` with a recomputed digest | `Malformed` (derived-root check) |
| payload swapped under a valid descriptor | `Corrupt` (payload hash) |

### 2.5 Store scope (Rust store + capability envelope)

| scope the capability names | emitted bytes | class |
|---|---:|---|
| provenance descriptor id | 4096 | `ok` |
| plain (mode-12) descriptor id | 0 | `Unauthorized` |
| ContentId + ObjectId | 0 | `Unauthorized` |
| another page's provenance descriptor | 0 | `Unauthorized` |
| no envelope | 0 | `Unauthorized` |
| mode-19 object through the mode-12 reader | 0 | `Corrupt` |
| mode-12 object through the provenance reader | 0 | `Corrupt` |

A raw `read_object` of the stored blob returns 5,509 bytes that start with
`WFT1` and are **not** the plaintext; a content-hash fetch therefore yields a
frame, never a decode. Two identical payloads under different adapters store
different descriptor ids (`9c8344b0…3b1a` vs `04f02327…24f7`) and different
provenance roots.

## 3. Cost

Receipts `bench/receipts/kv-provenance-20261109T181406Z.json` (cited) and
`bench/receipts/kv-provenance-20261109T181706Z.json` (repeat). BF16, 4 heads,
head_dim 64 (512 B/row), two adapters, one extension, 40 repetitions per cell.
Bench court, one host, warm cache.

| page | mode-12 frame | mode-19 frame | descriptor | overhead | overhead % |
|---:|---:|---:|---:|---:|---:|
| 1 KiB | 1,858 B | 2,501 B | 909 B | **+643 B** | **+34.61 %** |
| 64 KiB | 66,422 B | 67,066 B | 914 B | **+644 B** | **+0.97 %** |

| operation (p50) | 1 KiB | 64 KiB |
|---|---:|---:|
| encode, mode 12 | 266.9 µs | 508.2 µs |
| encode, mode 19 | 276.1 µs | 541.8 µs |
| decode, mode 12 | 11.1 µs | 159.0 µs |
| decode, mode 19 | 28.5 µs | 182.2 µs |
| descriptor validation alone | 16.9 µs | 19.0 µs |
| authorized store read, mode 12 | 35.0 µs | 280.5 µs |
| authorized store read, mode 19 | 53.0 µs | 301.8 µs |

Repeat run: decode 11.2 → 28.4 µs and 159.2 → 179.4 µs; descriptor 16.9 /
19.2 µs; authorized reads 35.0 → 53.4 µs and 281.4 → 300.2 µs. Peak RSS
5,948 KiB. The descriptor bytes are the same in both page sizes (the descriptor
does not contain page data), so the byte overhead is absolute and only its
percentage shrinks with page size.

## 4. Losses (all real, none hidden)

* **Small pages pay a large relative cost.** A 1 KiB page grows 34.6 % and its
  decode more than doubles, because descriptor validation (~17 µs) is the same
  fixed work either way.
* **Descriptor validation is not free** even at 64 KiB: ~19 µs per page, and it
  runs before any payload is touched.
* **The salt is not privacy.** It changes identifiers; it does not hide timing,
  sizes, access patterns, or the fact that KV storage happened. Anyone who can
  read the frame sees the salt and every dimension.
* **A raw object read returns the frame.** The store's KV path is the only
  decode path that enforces the scope; a caller holding the raw bytes and
  bypassing the store can still call the codec directly. The guarantee is about
  the store API, not about the bytes being unreadable.
* **No Go store parity.** The whole KV capability surface (`kv_capability.rs`,
  `kv_prefix.rs`) is Rust-only; WeftFS-Go has no KV page writer or reader. The
  Go codec implements mode 19 and the adversarial codec cases only.
* **No CLI verb** for provenance-bound KV writes; the store API and the
  conformance vectors are the surface. The mode-12 `weft kv` CLI is unchanged.
* **No real serving integration or time-to-first-token claim.** Pages are
  synthetic; no vLLM/SGLang is in the court.
* **Not added to `verify-all`.** The adversarial court is a normal Rust test
  (`tests/kv_provenance.rs`) covered by `cargo test --all-targets`; the bench is
  run manually, so the aggregate receipt does not include it.
* The Go codec's `resolveScoped` court case is a codec-level model (it decodes
  then compares roots); the store-level ordering guarantee is demonstrated only
  in Rust.

## 5. Exactness and gates

* Every court assertion also checks decoded bytes equal the input; no run in
  this document was accepted without that check.
* WeftCodec `cargo test --all-targets`: 75 unit tests plus every integration
  binary, 0 failed. New `tests/kv_prov.rs`: 9/9, including replay of all 20
  committed vectors.
* WeftCodec-Go `gofmt -l` (empty), `go vet`, `CGO_ENABLED=0 go build`, and
  `go test -race -count=1 ./...`: all packages pass. New conformance test
  replays the same 20 vectors and 4 adversarial codec cases.
* WeftFS `cargo test --all-targets`: 695 passed, 0 failed, 0 ignored.
* FUSE court `cargo test --features fuse --all-targets`: 738 passed, 0 failed,
  2 pre-existing ignored.
* Cross gate: `PASS rust->go kv_prov_p0`, `PASS go->rust kv_prov_p0`,
  `PASS identical kv_prov_p0`, `YES byte-identical kv_prov_p0`,
  `cross-language gate: all checks passed`.
* Conformance: manifest 376 entries, aggregate
  `9fb69ebfba3f5b7b1d22c843bec0a064a3b43665c391b90cfe4f36be58a91177`; coverage
  `19:8/12`; Rust adapter 222 pass / 0 fail / 33 skip; Go adapter 222 / 0 / 33
  (frame kind).
* Spec drift: 48 file comparisons across three mirrors.

## 6. Reproduction

```sh
# codec gates
bash scripts/docker-court.sh rust "cd /work/WeftCodec && cargo test --all-targets"
bash scripts/docker-court.sh go   "cd /work/WeftCodec-Go && go test -race -count=1 ./..."

# cross gate (four courts, as verify-all runs it)
bash scripts/docker-court.sh rust "cd /work/WeftCodec && cargo build --release --quiet && cd /work/WeftCodec-Go && bash hack/cross.sh rust-gen"
bash scripts/docker-court.sh go   "cd /work/WeftCodec-Go && bash hack/cross.sh go-build && bash hack/cross.sh go-gen && bash hack/cross.sh go-decode"
bash scripts/docker-court.sh rust "cd /work/WeftCodec-Go && bash hack/cross.sh rust-decode"
bash scripts/docker-court.sh go   "cd /work/WeftCodec-Go && bash hack/cross.sh compare"

# adversarial court
bash scripts/docker-court.sh rust "cd /work && cargo test --test kv_provenance -- --nocapture"

# cost
bash scripts/docker-court.sh bench "cd /work && cargo build --release --example kv_provenance_bench \
  && .docker-cache/target-rust/release/examples/kv_provenance_bench --out /work/bench/receipts/kv-provenance-<stamp>.json"
```
