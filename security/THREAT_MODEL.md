# WeftFS Threat Model (v1, living document)

**Status:** Initial model for the v1 single-host store. It states what is
mitigated, what is partially mitigated, and what is explicitly not claimed.

## Assets

- Exact file bytes and their immutable content identities.
- Object identity, versions, snapshots, and relation history.
- Authorization state (POSIX mode, ownership, `weft.acl.read`).
- Dependency closure required for lossless reconstruction.
- Metadata confidentiality (existence, counts, relationships).

## Trust boundaries

1. **Kernel ↔ FUSE daemon.** The kernel supplies `uid`, `gid`, and `groups`;
   the daemon must not trust client-supplied path strings beyond kernel-normalized
   lookup semantics.
2. **Store ↔ untrusted files on disk/import.** Commit records, frames,
   manifests, and selector text are parsed as hostile input.
3. **Principal ↔ principal on one host.** v1 is store-local; cross-tenant
   isolation is not claimed.
4. **Context graph ↔ codec dependencies.** The graph may cycle; the codec
   dependency graph must not.

## Threats and controls

| Threat | Control | Status |
|---|---|---|
| Forged/corrupt frame lengths, offsets, counts | checked arithmetic, table invariants, limits, trailer CRC, hash verification | implemented (codec) |
| Decompression bomb | declared output length, zstd window cap, `max_decoded_bytes` | implemented (codec) |
| Delta bomb / overlap abuse | COPY bounds checked against base length, op/byte limits | implemented (codec) |
| Dependency substitution | resolver verifies hash **and** length; missing/denied is typed failure; no fallback | implemented (codec) |
| Dependency cycle | visiting set in frame DAG decode; depth bound | implemented (codec) |
| Torn commit / partial superblock | dual slots, CRC, commit CRC+hash, fallback generation | implemented (store, tested by corruption) |
| Path traversal / `..` | FUSE/Kernel lookup uses entry names; NUL and `/` rejected in names; no host path is derived from client strings | store level implemented; FUSE tests planned |
| Symlink escape beyond mount | return symlink target as bytes; kernel resolves within its namespace; never `open` a host path from a symlink | planned with FUSE |
| Selector query DoS | input/nesting/operand limits, authorized universe, no recursion in v1, offset cost noted | parser implemented; resolver planned |
| Selector ambiguity | cardinality exposed; `require_one` rejects 0/n | plan (FS) |
| Unauthorized object disclosure via counts/evidence | authorization filter before matching; evidence filtered | plan (FS) |
| Cross-object dedup side channel | v1 dedups within one store only; no cross-tenant store, no claim of dedup privacy | documented limitation |
| Malicious import/export | parse as untrusted, verify all hashes, bounded archive, no path extraction outside destination | planned |
| GC deleting live dependency | mark from snapshots, tips, checkpoints, handles; grace period; never delete reachable hash | plan (FS) |
| Stale/duplicate object hash with wrong length | length + hash re-verified on read | implemented (codec); store read path to test |
| Lock bypass / concurrent writers | `flock(LOCK_EX)`; open fails `Busy`; no distributed claim | plan (FS) |
| Unauthorized relation traversal | relation visible only if subject and target visible | plan (FS) |

## Not claimed in v1

- Confidentiality of stored bytes at rest (no encryption).
- Authentication of authorship; BLAKE3 is unkeyed integrity, not a signature.
- Cross-tenant isolation, multi-writer consistency, replication, or remote
  object stores.
- Resistance to a privileged local attacker who can read the store directly.
- Constant-time behavior for authorization decisions.
- Deduplication privacy across principals or namespaces.

## Security-relevant invariants

- A hash mismatch never yields bytes.
- A missing dependency never yields substitute content.
- An ambiguous selector never silently returns one object.
- No API returns an object the principal cannot read, including counts.
- FUSE must not expose a host path traversal primitive.

## Verification status

Codec controls are tested in both Rust and Go conformance suites. Store controls
are exercised by unit, crash-proxy, and corruption tests as P4/P5 land. This
document is updated when a control moves from planned to implemented; it is not
a claim that planned controls exist.
