# WeftFS v1.4.1 — measured augmentation numbers, corrected prose, no wire change

Released on `staging` and merged to `main`. v1.4.1 is the first release after
v1.4.0. It changes **no frozen wire or on-disk byte**: the WFT1 frame, payload
modes 0–20, canonical WCB, `weft.manifest.v1`–`v4`, the selector grammar, the
store layout, the superblock and recovery protocol, the transport container,
`weftfs.commit.v1` / `v2` and every frozen store and conformance fixture are
unchanged. No mode value was added, removed, or renumbered. Package versions are
bumped so the registry artifacts name this tree.

Everything was built and measured in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

**Date:** 2026-11-13 · **Branch:** `staging` merged to `main` · **Tag:** `v1.4.1`.

Every current performance number in this note is emitted from a receipt between
`BEGIN GENERATED` sentinels by `scripts/perf_numbers.py`; the `perf_numbers`
court fails closed when a number drifts from its receipt. The canonical table is
[`PERF-NUMBERS.md`](PERF-NUMBERS.md).

## The short version

<!-- BEGIN GENERATED:release-v141-short (scripts/perf_numbers.py) -->
| Question | Answer |
|---|---|
| How does the shipped write compare with the last release's headline? | The shipped planned write is **974.1 ms** least-contended / 1,175.3 ms p50 at 282,106,049 B. The R3 contract measured 24,840.1 ms at 284,139,539 B (stamp 20261109T134244Z, revision 2fb4e4b7fa5e); the intermediate rungs are in §"Write ladder". |
| Did it reach native parity? | **No.** Native A is 327.2 ms, so the shipped write is **2.98× A** for 33.53 % fewer bytes. The separately measured frontier is 592.6 ms (1.78× A) but no shipped CLI verb reaches it. |
| Did any byte change? | **No.** No frozen wire, on-disk, manifest, commit, index, identity or conformance byte changed. Every planned row publishes 282,106,049 B in the R25 court and restores 101/101 exactly. |
<!-- END GENERATED:release-v141-short -->

## What changed

### R25 — the contract court re-measured, and every row says how it is reproduced

The frozen contract ([`bench/CONTRACTS-augmentation.md`](../bench/CONTRACTS-augmentation.md))
already required seven interleaved, rotated repetitions, but the court had been
running one pass per arm. It now runs seven, rotates the arm order by one
position per repetition, and publishes every raw repetition. Each
`configurations.<row>.reproduction` object records `shipped_default`,
`requires_flag` and `reproduces_with`, so no reader can mistake an ablation or
a harness composition for the product's default path.

The court also accepts a verified alternate fixture
(`WEFT_AUG_FIXTURE`, `WEFT_AUG_FIXTURE_SHA256`, `WEFT_AUG_MAX_OTHERS`,
`WEFT_AUG_INCLUDE_EMBEDDING`, `WEFT_AUG_LABEL`), which is how the cross-family
rows below run the same frozen contract without substituting a container.

Receipts: `augmentation-local-20261113T004000Z.json` (A/N/B/C/D/E, local POSIX),
`augmentation-minio-20261113T010500Z.json` (A and C over MinIO),
`augmentation-local-20261113T011500Z.json`, `…T012000Z.json` (pythia-70m step
0/8) and `…T013000Z.json` (Qwen2.5-3B-Instruct). Every receipt names its own
stamp and the revision it ran at.

The host was shared during the local run and several cells contain multi-second
durability stalls. Every sample is published; the least-contended repetition is
used for phase attribution (contract §6.10), and the median and p95 are
published unchanged. The table below is generated from the receipt, not from
this paragraph.

### Rule 13 — the comparator is the same backend, not the reference filesystem

`AGENTS.md` rule 13 makes the comparator explicit. Every write result in this
repository is **one backend with and without** the WeftFS protocol + codec
(arms A native, N native + manifest, B protocol pass-through, C codec, D
protocol, E combined), with matched durability, consistency and workload. The
Rust/Go reference filesystem is development provenance and is never the subject
of a headline comparison, and no row pits it against ext4, XFS, ZFS, Btrfs,
3FS, Lustre, CephFS, JuiceFS, DAOS, Weka or VAST. Reference-filesystem courts
that already existed (whole-disk ZFS/Btrfs and ext4/XFS `fio`) are now labelled
provenance-only in `README.md` and `bench/RESULTS-comparative-v1.md` and are
excluded from every claim.

### Documentation corrections

* The paper's abstract no longer leads with the pre-R17 R3 measurement. It
  leads with the current shipped result and names the frontier separately,
  stating plainly that native parity is not reached. The R3 number survives
  only as a historical quotation naming its receipt stamp and revision.
* `python/README.md` no longer says PyPI serves 1.2.0 or that 1.4.0 was not
  uploaded. 1.4.0 is live; the file now says what PyPI serves and where the
  artifact hashes live.
* `docs/RELEASE-v1.4.0.md` no longer says "Nothing was uploaded"; the live
  registry state is recorded there and in the artifact receipt.
* `scripts/publish-pypi.sh` no longer deletes an operator-supplied
  `WEFT_PYPI_TOKEN_FILE`. It stages a private 0600 copy and deletes only that
  copy. It also accepts `PYPI_TOKEN` directly.

## Write ladder

All rungs write the same 424,439,296 logical bytes on the same local POSIX
backend under the same court limits. "× native A" divides each rung by its own
native control. The R21/R22 rows measure a prepare/publish pipeline that no
shipped CLI verb reaches; they are a store-API frontier, not the product
default.

<!-- BEGIN GENERATED:release-v141-ladder (scripts/perf_numbers.py) -->
| Rung | Stamp | Planned write (ms, least-contended) | Footprint (B) | × native A | Shipped CLI reaches it? |
|---|---|---:|---:|---:|---|
| R3 | `20261109T134244Z` | 24,840.1 | 284,139,539 | 73.96× | yes: sequential planned writer, one commit per tensor |
| R17 | `20261112T054124Z` | 24,229.2 | 282,283,011 | 74.22× | yes: same writer, cost attributed |
| R18 | `20261112T081151Z` | 4,117.8 | 282,283,011 | 12.56× | yes: two-stage planner |
| R19 | `20261112T100622Z` | 2,328.4 | 282,283,011 | 7.06× | yes: mode-9 encoder |
| R20 | `20261112T125513Z` | 1,236.8 | 282,106,049 | 3.65× | yes: group commit + deferred barrier |
| R21 | `20261112T155655Z` | 623.3 | 282,106,049 | 1.87× | **no**: pipeline store API |
| R22 | `20261112T202816Z` | 592.6 | 282,106,049 | 1.78× | **no**: pipeline store API |
| R25 (current) | `20261113T004503Z` | 974.1 | 282,106,049 | 2.98× | **yes: this is the shipped default** |
<!-- END GENERATED:release-v141-ladder -->

The ≤ 336 ms rung set in the earlier plan was **not reached**. The shipped
sequential writer is the R25 row; the frontier is a numerical claim about one
fixture.

## Cross-family rows

<!-- BEGIN GENERATED:release-v141-families (scripts/perf_numbers.py) -->
| Fixture | Logical bytes | A native (ms) | C shipped (ms) | × A | C footprint (B) | Fewer bytes than A |
|---|---:|---:|---:|---:|---:|---:|
| Qwen2.5-0.5B-Instruct, 101 tensors | 424,439,296 | 327.2 | 974.1 | 2.98× | 282,106,049 | 33.53 % |
| Qwen2.5-3B-Instruct, 9 tensors | 626,540,544 | 411.6 | 1,329.1 | 3.23× | 415,882,220 | 33.62 % |
| pythia-70m step 0, 76 tensors | 281,706,496 | 233.3 | 12,008.6 | 51.48× | 163,964,176 | 41.80 % |
| pythia-70m step 8, 76 tensors | 281,706,496 | 226.8 | 12,070.4 | 53.22× | 163,830,373 | 41.84 % |
<!-- END GENERATED:release-v141-families -->

The pythia rows are a real loss: the 32-bit `FLOAT_TENSOR` encode dominates and
the codec row is roughly fifty times the native wall. The 3B row is a nine-tensor
626 MB subset, not the whole model. The bytes saved are consistent; the write
multiplier is not.

## Verification

<!-- BEGIN GENERATED:release-v141-verify (scripts/perf_numbers.py) -->
`bench/receipts/verify-20261113T022751Z.json`: **46/46** courts pass,
0 failed, 0 skipped, at WeftFS `e67e63186456d8d0915204baa19d65013a6bb281`.
Rust all-targets **807** passed / 0 failed; FUSE all-targets
**850** passed / 0 failed / 2 ignored
(the two pre-existing kernel/`CAP_SETUID` tests); WeftCodec **290** passed.
The conformance manifest holds **420** vectors (aggregate
`6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`); the claims manifest holds **1,046** entries and
`scripts/check-claims.sh` passes; `scripts/perf_numbers.py --check` passes.
<!-- END GENERATED:release-v141-verify -->

## Artifacts and registry state

<!-- BEGIN GENERATED:release-v141-artifacts (scripts/perf_numbers.py) -->
Built in the python court from the release tree and checked with `twine check`
(both pass):

| file | size | sha256 |
|---|---:|---|
| `weftcodec-1.4.1-py3-none-any.whl` | 115,889 B | `92efd39d2e4732081dccfccbb19c2f2849ff427e9f38d6aed9226624d5ceed06` |
| `weftcodec-1.4.1.tar.gz` | 156,971 B | `773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2` |

The exact registry state, including upload times and checksums, is in
`bench/receipts/release-artifacts-20261113T011500Z.json`. If a PyPI token is
available the same artifacts are uploaded and the live JSON API is re-read; if it
is not, the receipt names that blocker and no upload is claimed.

Previously live: `weftcodec 1.4.0` wheel `41ad31b43326feb86ef9cc2cac6b5d4f369e19fe6d9ce0da80203c940ff9d3c6`
(115,851 B) and sdist `2fd174fc74ba4bba3c2d1dff241c9eeb9f4f002dd25af994b48f04ae201e69c0`
(156,858 B); `weftcodec 1.4.0` crates.io checksum
`7f24d38382f7894903cc04763493530b48a5cc13d8e4b66fa08e0f82ea756882`; `weftfs 1.4.0` crates.io checksum
`d94ff6e7803a7ae7af807c5b4fa162b60f1002fa1b3388e086a63562a4d06be0`.
<!-- END GENERATED:release-v141-artifacts -->

## Losses and non-claims

* Native write parity is **not** reached by the shipped writer; the frontier row
  is not the default.
* The pythia-70m rows are roughly fifty times the native wall.
* The write ladder's R21/R22 rungs are not reachable from a shipped CLI verb.
* The host was shared; some cells contain multi-second durability stalls. All
  samples are published and the least-contended repetition is used for
  attribution.
* MinIO is one loopback process on a tmpfs. No media, power-loss, fleet, cloud,
  GPU, or multi-node claim is made.
* No frozen wire or on-disk byte changed; this release is measurement, prose and
  packaging.

## Where to read next

* [`bench/RESULTS-augmentation.md`](../bench/RESULTS-augmentation.md) — the
  contract court's method and every cell.
* [`docs/PAPER.md`](PAPER.md) — the claims, the ladder, the cross-family rows and
  the threats to validity.
* [`docs/PERF-NUMBERS.md`](PERF-NUMBERS.md) — every published number with its
  receipt, stamp and revision.
* [`docs/RELEASE-v1.4.0.md`](RELEASE-v1.4.0.md) — the previous release.

## Published state (post-tag observation)

Recorded in `bench/receipts/release-artifacts-20261113T032500Z.json`.

| Repo | Merge commit | Tag |
|---|---|---|
| WeftFS | `abe4d989bb1e52524c7956ac850c2dece78617ca` | `v1.4.1` |
| WeftCodec | `60f32fce1bb9408b6434942501d67d948d58c87f` | `v1.4.1` |
| WeftCodec-Go | `20c320c91056db6a8c6c940f4ce345c08c60dd83` | `v1.4.1` |
| WeftFS-Go | `67fb2fc7a69017eb93789e446e4a608de9ee495b` | `v1.4.1` |

GitHub releases: [WeftFS](https://github.com/infinityabundance/WeftFS/releases/tag/v1.4.1),
[WeftCodec](https://github.com/infinityabundance/WeftCodec/releases/tag/v1.4.1),
[WeftCodec-Go](https://github.com/infinityabundance/WeftCodec-Go/releases/tag/v1.4.1),
[WeftFS-Go](https://github.com/infinityabundance/WeftFS-Go/releases/tag/v1.4.1).
The WeftFS release carries the wheel and sdist as assets.

crates.io published `weftcodec 1.4.1` (checksum
`a83e08ef4db2ec68b7f28ad28aa622f23d873fdc65d6c74cd2d20c6c0014283f`,
3,020,483 B) and `weftfs 1.4.1` (checksum
`85870e61e5e7d4c1cf75ad9ed126fd6eacfc77be7315b59a129c31e711ead73c`,
1,850,794 B), both `yanked=false`.

GHCR `:v1.4.1` and `:latest` were read live and are the same image-index digest
per image: `weftfs` `sha256:717683c1…f1f46`, `weftcodec`
`sha256:4c6f8586…ee886`, `weftcodec-go` `sha256:80239779…e592f`,
`weftfs-go` `sha256:b36e36d6…48c27`. A rebuild is not bit-reproducible.

**PyPI 1.4.1 is not published.** No token was available to the release session
(no `PYPI_TOKEN`, no `WEFT_PYPI_TOKEN_FILE`, no `.docker-cache/.pypi-token`), so
`scripts/publish-pypi.sh` refused with `publish-pypi: no token available`. PyPI
still serves `weftcodec 1.4.0` with the live long description that this release
corrects; the 1.4.1 wheel and sdist are built, `twine check`-clean and attached
to the GitHub release. `scripts/publish-pypi.sh` no longer deletes an
operator-supplied token file, so an operator can now point
`WEFT_PYPI_TOKEN_FILE` at a persistent file and upload without the script
removing it.

**Artifact-hash note.** The generated artifact table earlier in this file was
written at the tag and names the first 1.4.1 build. `python/README.md` is the
long description, so it must not name the wheel's own digest; removing that
self-reference changed the description and therefore the next build. The
attached GitHub-release assets are the **final** build — wheel
`92efd39d2e4732081dccfccb19c2f2849ff427e9f38d6aed9226624d5ceed06` (115,889 B)
and sdist `773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2`
(156,971 B) — and those hashes are the authoritative ones in
`bench/receipts/release-artifacts-20261113T032500Z.json`. A rebuild is not
bit-reproducible; the attached asset and the receipt win.
