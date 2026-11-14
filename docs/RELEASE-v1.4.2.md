# WeftFS v1.4.2 — packaging correctness: the long description no longer describes its own upload

Released on `staging` and merged to `main`. v1.4.2 changes **no frozen wire or
on-disk byte**: the WFT1 frame, payload modes 0–20, canonical WCB,
`weft.manifest.v1`–`v4`, the selector grammar, the store layout, the superblock
and recovery protocol, the transport container, `weftfs.commit.v1` / `v2` and
every frozen store and conformance fixture are unchanged. No mode value was
added, removed, or renumbered. No code path, planner rule, encode/decode
implementation or measurement method changed.

It is a **packaging and documentation correctness** release. Package versions
are bumped so the registry artifacts name this tree; the substance is that the
Python wheel's long description stopped describing its own publication state,
and a court now rejects that class of statement.

Everything was built and checked in the Docker courts with the hard limits in
`AGENTS.md` (memory == swap, pids, CPUs, bounded `/tmp`, `no-new-privileges`,
`cap-drop=ALL`). Nothing was built or tested on the host.

**Date:** 2026-11-13 · **Branch:** `staging` merged to `main` · **Tag:** `v1.4.2`.

Every current number in this note is emitted from a receipt between
`BEGIN GENERATED` sentinels by `scripts/perf_numbers.py`; the `perf_numbers`
court fails closed when a number drifts from its receipt. The canonical table is
[`PERF-NUMBERS.md`](PERF-NUMBERS.md).

## What was wrong

The wheel's long description is `python/README.md`, baked into the wheel at
build time. Each release so far wrote a block that described its own upload
state, and that block was false the moment the artifact was uploaded:

* the live 1.4.0 text said that PyPI served an older version and that the 1.4.0
  wheel and sdist were not uploaded because a token did not exist;
* the live 1.4.1 text said PyPI served 1.4.0 and pointed at a release note for
  the 1.4.1 hashes that it could not contain itself.

An artifact cannot truthfully describe its own publication status. The sentence
becomes false at the instant of upload, and because the description is an input
to the build, it also makes every rebuild differ. The 1.4.1 note already needed
a post-hoc "artifact-hash note" to explain why the digest it named was not the
digest a reader downloaded: the same defect, twice.

## The structural fix

`python/README.md` now makes no version-specific publication claim. Its install
commands are unpinned, and one stable paragraph replaces the generated block.
The paragraph, verbatim:

> `weftcodec` is published on PyPI as
> [`weftcodec`](https://pypi.org/project/weftcodec/) and on crates.io as
> [`weftcodec`](https://crates.io/crates/weftcodec), with
> [`weftfs`](https://crates.io/crates/weftfs) for the filesystem. This file is
> baked into the wheel as its long description, so it names no version and no
> digest: the current version, file names, sizes and SHA-256 values are on those
> project pages and, for every release, in the `docs/RELEASE-v*.md` notes in the
> [source repository](https://github.com/infinityabundance/WeftFS/tree/main/docs).

The `python-readme-current` generated block and its manifest entry are deleted.
The description contains no version literal, no artifact file name and no
digest, so the next release does not have to amend it.

## A guard, not another wording pass

`scripts/check_long_description.py` fails closed on the whole class of
statement, not on one phrasing of it. It rejects:

| rule | what it rejects |
|---|---|
| `pinned-install` | `pip install weftcodec==<version>`, including inside a quoted extra such as `"weftcodec[torch]==<version>"` |
| `local-artifact-install` | `pip install dist/...` or `pip install ./...` |
| `wheel-path-install` | any `pip install` naming a `.whl` file |
| `registry-serves` | any claim about what PyPI or crates.io serves, hosts, holds or ships |
| `upload-state` | "not uploaded", "not published", "nothing was uploaded", "pending upload", "until the upload", "upload is unblocked", a missing token, and "not on PyPI/crates.io" |
| `package-version` | a version literal bound to `weftcodec`: `weftcodec 1.4.2`, `weftcodec==1.4.2`, `weftcodec-1.4.2-py3-none-any.whl`, `.../weftcodec/1.4.2/` |
| `digest-literal` | any literal 64-hex SHA-256 value |

It also requires the three stable statements that replace the old block: an
unpinned `pip install weftcodec`, the PyPI project URL, and the crates.io crate
URL. Deleting the replacement is a failure, not a silent pass.

`scripts/check_long_description_court.sh` gives it the aggregate's explicit skip
semantics (exit `78` with one printed reason on a host with no Python >= 3.8),
and it is registered in `scripts/verify-all.sh` as the `long_description` court.
The old text fails it and the new text passes; the demonstration is in the
receipt `bench/receipts/long-description-court-20261113T045732Z.json`, beside
that receipt's raw outputs.

## The three 1.4.1 builds

`weftcodec 1.4.1` exists as three distinct builds with different digests. None
of them is "the" 1.4.1 artifact, and none of the numbers below is presented as
one:

<!-- BEGIN GENERATED:release-v142-builds141 (scripts/perf_numbers.py) -->
| 1.4.1 build | where a reader gets it | wheel size | wheel sha256 | sdist size | sdist sha256 |
|---|---|---:|---|---:|---|
| tagged release note | nowhere: superseded before upload, never attached | 115,540 B | `74779b5c26feaaf0dbb70c909d48221a699cc4e91abd5fbd6c1eed7782bcb176` | 156,293 B | `9fa6cfb62062754e13db98708dd32727b9d046dd6ab5896569216958097b79a0` |
| GitHub release asset | the `v1.4.1` release assets | 115,889 B | `92efd39d2e4732081dccfccbb19c2f2849ff427e9f38d6aed9226624d5ceed06` | 156,971 B | `773c317d216b252be36dc6b07d5a1add8ba80c14faf46c7e510011180ec196e2` |
| PyPI | `pip install weftcodec==1.4.1` | 115,889 B | `73cd3422ec7267b9876ad48458682e6d7dede24a53b4c33c76d50dc8613b4f32` | 157,012 B | `93a0b8c8c320f8d3316152c2bdc1b797f4869fcb2116dbbb825953b8942286a1` |
<!-- END GENERATED:release-v142-builds141 -->

The first build is the one the tagged 1.4.1 release note names; it was superseded
before it was attached or uploaded. The second is what the `v1.4.1` GitHub
release carries. The third is what `pip install weftcodec==1.4.1` downloads. The
first-to-second difference is a real content change (the long description
stopped naming the digests); the second-to-third difference is build
non-reproducibility, not a content change, and it is why a Python artifact here
must be identified by digest rather than by source tree.

## Verification

<!-- BEGIN GENERATED:release-v142-verify (scripts/perf_numbers.py) -->
`bench/receipts/verify-20261113T033408Z.json`: **47/47** courts pass,
0 failed, 0 skipped, at WeftFS `52da9888f3726e6af5c364af7707fd98574fc954`.
Rust all-targets **807** passed / 0 failed (170 result lines);
FUSE all-targets **850** passed / 0 failed /
2 ignored (the two pre-existing kernel/`CAP_SETUID` tests);
WeftCodec **290** passed. The conformance manifest holds
**420** vectors (aggregate `6287b817b94d2cc805abe23561d3d4270cd46b3c2e4f4e598fa673dfe48dfbe8`);
**1,190** claims are audited; `scripts/check-claims.sh`,
`scripts/perf_numbers.py --check`, spec drift, the conformance manifest/coverage
and the new `long_description` court all pass.
<!-- END GENERATED:release-v142-verify -->

Independently of the aggregate, the released wheel was installed into a fresh
venv in the python court and the Python suite was run from the installed wheel
rather than the source tree. The receipt
`bench/receipts/python-installed-wheel-20261113T050840Z.json` records the
counts, the run time and the installed wheel's digest, and confirms the digest
is the uploaded build: the run installs `python/dist/` and deliberately does
not rebuild it, because a Python rebuild here is not bit-reproducible.

## Artifacts and registry state

<!-- BEGIN GENERATED:release-v142-artifacts (scripts/perf_numbers.py) -->
Built once in the python court from the release tree and checked with `twine check`
(both pass):

| file | size | sha256 |
|---|---:|---|
| `weftcodec-1.4.2-py3-none-any.whl` | 115,685 B | `787cc6ca203ea6a6c6285551281833de44b826946968ba3faf3b3d2d9ef2b022` |
| `weftcodec-1.4.2.tar.gz` | 156,563 B | `ab214bffced0312f0f50fb3a1929a68c2163e32493f9a159821a9ea78b39ae78` |

Built once at WeftFS `52da9888f372` and not rebuilt after this note
was committed. The wheel's `METADATA` description is byte-identical to
`python/README.md` and contains no registry-state sentence, no pinned install
version and no 64-hex digest. Python builds here are not bit-reproducible, so this
table is the build this note records, not a reproducibility proof. An upload that
rebuilds it must record the new digests separately. Receipt:
`bench/receipts/release-artifacts-20261113T045609Z.json`.
<!-- END GENERATED:release-v142-artifacts -->

crates.io: `cargo publish --dry-run` packages and verifies `weftcodec 1.4.2` in
the rust court; the `weftfs 1.4.2` dry run cannot resolve `weftcodec 1.4.2` from
the index until `weftcodec` is published, exactly as in 1.4.0 and 1.4.1 — the
real run publishes `weftcodec` first, waits for the index, then publishes
`weftfs`. Neither crate was published by this session.

**Uploading these bytes.** `scripts/publish-pypi.sh --from-dist` uploads the
existing `python/dist/` artifacts and twine-checks them first; it does not
rebuild. That mode exists because a Python rebuild here is not bit-reproducible,
so it is the only way to make the uploaded bytes the recorded bytes. Plain
`scripts/publish-pypi.sh` still rebuilds, and its output must then be recorded
as its own build — the mistake that produced three 1.4.1 artifacts.

PyPI: neither 1.4.2 artifact was uploaded by this session. The coordinator
performs the upload and then records the live state; until then
`https://pypi.org/pypi/weftcodec/1.4.2/json` returns `404`, and the project page
still serves the 1.4.1 artifacts whose long description is the pre-fix text.

## Losses and non-claims

* **The published 1.4.0 and 1.4.1 descriptions stay wrong until 1.4.2 is
  uploaded.** A release cannot retroactively fix an uploaded artifact; only a
  new upload replaces the project description.
* **Python builds are not bit-reproducible here.** The same source tree yields
  different wheel and sdist bytes. Each artifact is identified by digest, which
  is why the 1.4.1 section has to name three builds.
* **The guard is a rule set, not a proof.** It rejects the known class of
  self-describing statement and the specific patterns above. A future edit could
  still write a different false sentence about the project that no rule catches.
* **No measurement claim.** No code path changed, so the shipped-write result
  quoted in `README.md` and `docs/STATUS.md` is still the R25 measurement at its
  own receipt, stamp and revision; it was not re-measured for this release.
* **No format, protocol or durability change**, and no new power-loss, media,
  fleet, cloud, GPU or multi-node claim. No live Azure, AWS or GCS account was
  exercised.

## Where to read next

* [`docs/RELEASE-v1.4.1.md`](RELEASE-v1.4.1.md) — the previous release, now with
  the three-build artifact table.
* [`docs/RELEASE-v1.4.0.md`](RELEASE-v1.4.0.md) — the release before it.
* [`docs/PERF-NUMBERS.md`](PERF-NUMBERS.md) — every published number with its
  receipt, stamp and revision.
* [`python/README.md`](../python/README.md) — the long description itself, and
  `scripts/check_long_description.py` — the guard.

## Published state (post-tag observation)

Recorded in `bench/receipts/release-artifacts-20261113T050602Z.json`.

| Repo | Merge commit | Tag object | Tagged commit |
|---|---|---|---|
| WeftFS | `aac23a9a11e7` | `1c8fcc5b6381` | `aac23a9a11e7` |
| WeftCodec | `926410bd7572` | `0a12da9d8505` | `926410bd7572` |
| WeftCodec-Go | `20c320c91056` | `dc179363ceb6` | `20c320c91056` |
| WeftFS-Go | `480eb9e6a924` | `a1959f62c0f6` | `480eb9e6a924` |

GitHub releases: [WeftFS](https://github.com/infinityabundance/WeftFS/releases/tag/v1.4.2),
[WeftCodec](https://github.com/infinityabundance/WeftCodec/releases/tag/v1.4.2),
[WeftCodec-Go](https://github.com/infinityabundance/WeftCodec-Go/releases/tag/v1.4.2),
[WeftFS-Go](https://github.com/infinityabundance/WeftFS-Go/releases/tag/v1.4.2).
The WeftFS release carries the wheel and sdist, and `gh release view` reports
the same digests as the local build in the table above.

GHCR `:v1.4.2` and `:latest` were read live and are the same image-index digest
per image: `weftfs` `sha256:3a17098a…26141`, `weftcodec`
`sha256:b8893492…90680`, `weftcodec-go` `sha256:af6b30bd…14bdbe`, `weftfs-go`
`sha256:7b44ce77…ca1a03c`. The four tag workflows succeeded. A rebuild is not
bit-reproducible.

`WeftCodec-Go` carries no source change in this release; its tag names the tree
that was already merged from staging, so its codec is byte-identical to 1.4.1.

**Registry uploads were not performed by this session.** At this stamp
`https://pypi.org/pypi/weftcodec/1.4.2/json` and
`https://crates.io/api/v1/crates/weftcodec/1.4.2` both return `404`. The
coordinator performs the uploads and records the live state; until then the
project page still serves the 1.4.1 artifacts with the pre-fix long
description.

## Post-upload observation (dated 2026-11-13)

This section was added after the note above was committed. The coordinator
performed the uploads; the state below was **observed**, not inferred, from
`https://pypi.org/pypi/weftcodec/json`,
`https://crates.io/api/v1/crates/weftcodec`,
`https://crates.io/api/v1/crates/weftfs` and `gh release view v1.4.2` on
2026-11-13. No receipt was written for this observation; the commands above are
the evidence and a reader can re-run them.

| Surface | Observed |
|---|---|
| PyPI `weftcodec 1.4.2` | `info.version` is `1.4.2`; wheel sha256 `787cc6ca203ea6a6c6285551281833de44b826946968ba3faf3b3d2d9ef2b022`, sdist sha256 `ab214bffced0312f0f50fb3a1929a68c2163e32493f9a159821a9ea78b39ae78`, both `yanked: false`, upload times `2026-11-13T05:12:55Z` and `05:12:56Z` |
| PyPI long description | the replacement text required by `scripts/check_long_description.py`: no version literal, no artifact file name, no digest, no upload-state sentence |
| GitHub `v1.4.2` release assets | the same two digests as the PyPI files, `isDraft: false`, `isPrerelease: false` |
| crates.io `weftcodec 1.4.2` | checksum `e7739c46c599b60842ac24656b94691c2cf24a280a90db0843257a531067fede`, published `2026-11-13T05:13:04Z`, not yanked |
| crates.io `weftfs 1.4.2` | checksum `fb7e30a4585f5b581e317cd4bf0683cc4ed2cb1be391cce26d08be859e9e2efe`, published `2026-11-13T05:13:23Z`, not yanked |

The uploaded PyPI digests are byte-identical to the build recorded in the
generated table above, so for 1.4.2 the uploaded bytes are the recorded bytes
(unlike 1.4.1, which has three builds). The sentences in this note saying the
artifacts “were not uploaded by this session” and that the JSON endpoints
returned `404` were true at this note's own stamp and are retained as written.
The loss “the published 1.4.0 and 1.4.1 descriptions stay wrong until 1.4.2 is
uploaded” is resolved for the current project page; the older descriptions
remain in the registry's history and are not rewritten.

**Not verified from the environment that wrote this section.** GHCR package
versions: `gh api /users/infinityabundance/packages/container/<name>/versions`
returns HTTP 403 (“You need at least read:packages scope”), so this section
neither confirms nor contradicts the GHCR digests quoted above. Those digests
remain the release session's own observation, cited as such.
