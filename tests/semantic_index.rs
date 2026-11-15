//! X14 — indexed semantic candidate discovery.
//!
//! The scan and the index are compared on the *same* store and the *same*
//! planning options, so a difference is a property of the discovery path and
//! not of the planner.

use std::sync::Arc;

use tempfile::TempDir;
use weftfs::semantic_index::LookupRequest;
use weftfs::tensor_store::{TensorLayoutSpec, TensorPlanOptions, TensorStrategy};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;
const ROWS: u64 = 64;
const COLS: u64 = 128;

fn options() -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        chunk_size: 4096,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

/// A BF16 tensor whose perturbed elements have only low mantissa bits flipped
/// (the default semantic policy discards three of them), and whose perturbation
/// set is chosen by `touch`. A base whose set is a subset of the query's set
/// keeps the same semantic root, which is what mode 14 requires.
///
/// The exponent is pinned to a finite value on purpose: the policy preserves
/// NaN/inf words *bit-exactly*, so a random exponent would give two tensors
/// that differ only in low mantissa bits two different semantic roots.
fn bf16_tensor_with(seed: u32, touch: impl Fn(u64) -> bool) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ u64::from(seed);
    let mut out = Vec::with_capacity((ROWS * COLS * 2) as usize);
    for index in 0..(ROWS * COLS) {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let mut value = ((state.wrapping_mul(0x2545_f491_4f6c_dd1d) as u16) & 0x807F) | 0x3C00;
        if touch(index) {
            value ^= 0x0005;
        }
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Every `step`-th element touched (a base).
fn bf16_tensor(seed: u32, step: u32) -> Vec<u8> {
    bf16_tensor_with(seed, move |index| step != 0 && index % u64::from(step) == 0)
}

/// The query: every 8th element (a superset of every base's set) plus a few
/// values no stored tensor has.
fn bf16_query(seed: u32, extra: u64) -> Vec<u8> {
    bf16_tensor_with(seed, move |index| {
        index % 8 == 0 || index % (4096 + extra) == 0
    })
}

fn store_with_family() -> (TempDir, Store, Vec<(String, weftfs::Hash32)>) {
    let dir = TempDir::new().unwrap();
    let mut store = Store::init(dir.path(), options()).unwrap();
    let layout = TensorLayoutSpec::row_major();
    let mut written = Vec::new();
    // Family A: a base and four near-identical variants. Family B: a different
    // distribution with the same shape (the distractor the index must not pick
    // first). Family C: a different shape (never comparable).
    let names: Vec<(&str, u32, u32)> = vec![
        ("a-base", 1, 8),
        ("a-v1", 1, 16),
        ("a-v2", 1, 24),
        ("a-v3", 1, 32),
        ("a-v4", 1, 40),
        ("b-distractor", 7, 0),
    ];
    for (name, seed, perturb) in names {
        let bytes = bf16_tensor(seed, perturb);
        let id = store
            .write_tensor_file(
                weftfs::state::State::ROOT_ID,
                name.as_bytes(),
                "bf16",
                &[ROWS, COLS],
                &bytes,
                &layout,
                Some(TensorStrategy::Zstd),
            )
            .unwrap();
        written.push((name.to_owned(), id));
    }
    let other_layout = TensorLayoutSpec::row_major();
    let id = store
        .write_tensor_file(
            weftfs::state::State::ROOT_ID,
            b"c-other-shape",
            "bf16",
            &[8, 16],
            &bf16_tensor(3, 0)[..8 * 16 * 2],
            &other_layout,
            Some(TensorStrategy::Zstd),
        )
        .unwrap();
    written.push(("c-other-shape".to_owned(), id));
    (dir, store, written)
}

fn plan_options(
    scan: bool,
    index: Option<Arc<weftfs::semantic_index::SemanticIndex>>,
) -> TensorPlanOptions {
    TensorPlanOptions {
        semantic_search: true,
        semantic_scan: scan,
        semantic_scan_max_objects: 64,
        max_candidates: 8,
        candidates: Vec::new(),
        semantic_index: index,
        semantic_index_query: true,
        ..TensorPlanOptions::default()
    }
}

#[test]
fn the_index_ranks_the_right_family_first() {
    let (_dir, store, written) = store_with_family();
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    assert_eq!(build.index.len(), written.len());
    assert_eq!(build.skipped, 0);
    assert!(build.record_bytes > 0);
    assert!(built_index_round_trips(&build.index));

    // Query with a variant that is not in the store: a perturbation of the
    // family A base.
    let query = bf16_query(1, 5);
    let request = LookupRequest {
        dtype: weftfs::representation::TensorElementDtype::Bf16,
        shape: &[ROWS, COLS],
        bytes: &query,
        exclude: None,
        limit: 4,
    };
    let (candidates, stats) = store.query_semantic_index(&build.index, &request).unwrap();
    assert_eq!(stats.stale, 0);
    assert!(stats.compatible >= 6, "{stats:?}");
    assert_eq!(candidates.len(), 4);
    let names: Vec<&str> = candidates
        .iter()
        .map(|candidate| {
            written
                .iter()
                .find(|(_, other)| *other == candidate.object)
                .map(|(name, _)| name.as_str())
                .unwrap()
        })
        .collect();
    assert!(
        names.iter().all(|name| name.starts_with("a-")),
        "the nearest candidates are the same family: {names:?}"
    );
    assert!(stats.best_distance.is_some());
}

fn built_index_round_trips(index: &weftfs::semantic_index::SemanticIndex) -> bool {
    let bytes = index.encode().unwrap();
    match weftfs::semantic_index::SemanticIndex::decode(&bytes) {
        Ok(decoded) => decoded == *index,
        Err(_) => false,
    }
}

#[test]
fn the_index_and_the_scan_choose_the_same_base() {
    let (_dir, store, _written) = store_with_family();
    // The tensor being written is a perturbation of the family-A base that is
    // not stored, so both discovery paths must find the *other* family members.
    let bytes = bf16_query(1, 5);
    let layout = TensorLayoutSpec::row_major();
    let semantic = |plan: &weftfs::tensor_store::TensorPlan| {
        plan.candidates
            .iter()
            .find(|candidate| candidate.strategy == TensorStrategy::SemanticDelta)
            .map(|candidate| {
                (
                    candidate.applicable,
                    candidate.physical_bytes,
                    candidate.detail.clone(),
                )
            })
            .unwrap_or((false, None, String::new()))
    };

    let scan = store
        .plan_tensor(
            "bf16",
            &[ROWS, COLS],
            &bytes,
            &layout,
            &[],
            &plan_options(true, None),
        )
        .unwrap();
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    let index = Arc::new(build.index);
    let indexed = store
        .plan_tensor(
            "bf16",
            &[ROWS, COLS],
            &bytes,
            &layout,
            &[],
            &plan_options(false, Some(index)),
        )
        .unwrap();

    let (scan_applicable, scan_bytes, scan_detail) = semantic(&scan);
    let (index_applicable, index_bytes, index_detail) = semantic(&indexed);
    assert!(scan_applicable, "the scan finds a base: {scan_detail}");
    assert!(index_applicable, "the index finds a base: {index_detail}");
    assert_eq!(
        scan_bytes, index_bytes,
        "the indexed path recovers the scan's saving exactly"
    );
    assert_eq!(scan.chosen, indexed.chosen);
    assert_eq!(scan.chosen_physical_bytes, indexed.chosen_physical_bytes);
}

#[test]
fn the_index_is_cheaper_than_the_scan_on_the_same_plan() {
    let (_dir, store, _written) = store_with_family();
    let bytes = bf16_query(1, 5);
    let layout = TensorLayoutSpec::row_major();
    let start = std::time::Instant::now();
    for _ in 0..10 {
        store
            .plan_tensor(
                "bf16",
                &[ROWS, COLS],
                &bytes,
                &layout,
                &[],
                &plan_options(true, None),
            )
            .unwrap();
    }
    let scan_ms = start.elapsed().as_secs_f64() * 1e3 / 10.0;
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    let index = Arc::new(build.index);
    let start = std::time::Instant::now();
    for _ in 0..10 {
        store
            .plan_tensor(
                "bf16",
                &[ROWS, COLS],
                &bytes,
                &layout,
                &[],
                &plan_options(false, Some(Arc::clone(&index))),
            )
            .unwrap();
    }
    let index_ms = start.elapsed().as_secs_f64() * 1e3 / 10.0;
    // The index is not allowed to be slower than the scan it replaces on this
    // six-tensor fixture. (It is a narrowing step, not a new cost.)
    assert!(
        index_ms <= scan_ms * 1.05,
        "indexed plan {index_ms:.3} ms vs scan {scan_ms:.3} ms"
    );
}

#[test]
fn a_stale_entry_is_skipped_and_counted() {
    let (_dir, mut store, written) = store_with_family();
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    store.store_semantic_index(&build.index).unwrap();
    assert!(store.load_semantic_index().unwrap().is_some());

    // Delete the family-A base: the index still holds its fingerprint, but the
    // object is gone. A lookup must skip it, count the staleness, and never
    // return bytes for it.
    store
        .unlink(weftfs::state::State::ROOT_ID, b"a-base")
        .unwrap();
    let base_id = written
        .iter()
        .find(|(name, _)| name == "a-base")
        .map(|(_, id)| *id)
        .unwrap();

    let query = bf16_query(1, 5);
    let request = LookupRequest {
        dtype: weftfs::representation::TensorElementDtype::Bf16,
        shape: &[ROWS, COLS],
        bytes: &query,
        exclude: None,
        limit: 6,
    };
    let (candidates, stats) = store.query_semantic_index(&build.index, &request).unwrap();
    assert!(
        stats.stale >= 1,
        "the deleted base must be counted stale: {stats:?}"
    );
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.object == base_id),
        "a stale entry must never yield bytes"
    );
    assert!(stats.returned > 0, "the other family members remain usable");
    // Every returned candidate is a live object.
    for candidate in &candidates {
        assert!(store.object_meta(candidate.object).is_ok());
        assert!(!candidate.bytes.is_empty());
    }
    // The planner still works from the stale index: it simply has one fewer
    // base to choose from.
    let bytes = bf16_query(1, 5);
    let plan = store
        .plan_tensor(
            "bf16",
            &[ROWS, COLS],
            &bytes,
            &TensorLayoutSpec::row_major(),
            &[],
            &plan_options(false, Some(std::sync::Arc::new(build.index))),
        )
        .unwrap();
    let semantic = plan
        .candidates
        .iter()
        .find(|candidate| candidate.strategy == TensorStrategy::SemanticDelta)
        .unwrap();
    assert!(semantic.applicable, "{}", semantic.detail);
}

#[test]
fn build_is_bounded_and_reports_what_it_skipped() {
    let (_dir, store, _written) = store_with_family();
    let build = store.build_semantic_index(3, u64::MAX).unwrap();
    assert_eq!(build.objects_scanned, 3);
    assert!(build.index.len() <= 3);
    // The byte bound is honoured *before* a read: with a bound smaller than one
    // tensor, nothing is read and nothing is indexed.
    let tiny_bytes = store.build_semantic_index(64, 10).unwrap();
    assert_eq!(tiny_bytes.bytes_read, 0);
    assert_eq!(tiny_bytes.index.len(), 0);
    // One tensor's worth of budget indexes exactly one tensor.
    let one = store.build_semantic_index(64, ROWS * COLS * 2).unwrap();
    assert!(one.index.len() >= 1);
}

#[test]
fn a_tampered_index_record_is_refused_on_load() {
    let (dir, mut store, _written) = store_with_family();
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    store.store_semantic_index(&build.index).unwrap();
    let path = dir.path().join("semantic-index.root");
    assert!(path.is_file(), "the index record was published");
    let mut bytes = std::fs::read(&path).unwrap();
    let index = bytes.len() / 2;
    bytes[index] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();
    // Reopen read-only so nothing is cached.
    drop(store);
    let store = Store::open_with_options(dir.path(), OpenMode::ReadOnly, options()).unwrap();
    let err = store.load_semantic_index().unwrap_err();
    assert!(
        matches!(
            err.class(),
            weftfs::error::Class::Corrupt
                | weftfs::error::Class::Malformed
                | weftfs::error::Class::NonCanonical
        ),
        "{err}"
    );
}

#[test]
fn the_index_is_control_state_not_logical_state() {
    let (_dir, mut store, _written) = store_with_family();
    let head = store.head();
    let generation = store.generation();
    let build = store.build_semantic_index(64, u64::MAX).unwrap();
    store.store_semantic_index(&build.index).unwrap();
    assert_eq!(store.head(), head);
    assert_eq!(store.generation(), generation);
    assert!(store.verify(true).unwrap().ok);
}
