//! T096 — store / fetch / reconstruct / recompute decision planner.
//!
//! Four declared strategies serve one logical tensor from a real WeftFS store:
//!
//! | strategy | what it does | contract |
//! |---|---|---|
//! | `fetch` | read the raw object | exact bytes retained |
//! | `decode` | read the mode-9 tensor object | exact frame decode |
//! | `reconstruct` | read the mode-11 delta object | exact dependency-chain reconstruction |
//! | `recompute` | run the declared operation over verified inputs | `determinism == exact` and every input verified |
//!
//! The contract gate is the **shipped** [`weftfs::provenance::choose_recovery`];
//! this harness does not re-implement it. An approximate candidate (a record
//! whose declared determinism is `declared` and whose execution differs from the
//! target) is always present in the candidate list and must never be selected
//! for an exact request.
//!
//! # What is measured
//!
//! For every request (one tensor under one availability mask) every available
//! strategy is executed and its wall time recorded. Policies then choose using
//! *only* the training traces; the held-out traces are evaluated once:
//!
//! * `best_fixed_priority` — the best fixed priority order over the four
//!   strategies, chosen on the training traces;
//! * `declared_cost` — the shipped planner's rule with declared costs;
//! * `mean_utility` — predicted cost per strategy = its mean observed cost on the
//!   training traces; choose the cheapest available;
//! * `oracle` — the cheapest observed strategy per request.
//!
//! Reported: total observed wall, regret against the oracle, the fraction of
//! requests where `recompute` was chosen **and** produced the exact bytes, and
//! the number of approximate candidates refused.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::json;

use weftfs::identity::{self, Hash32};
use weftfs::provenance::{
    choose_recovery, Determinism, ProvenanceEnvironment, ProvenanceInput, ProvenancePrincipal,
    ProvenanceRecord, RecoveryCosts, RecoveryStrategy,
};
use weftfs::safetensors::SafetensorsFile;
use weftfs::store::{Options, Store};
use weftfs::tensor_store::{TensorLayoutSpec, TensorStrategy, TensorWriteSpec};

const STRATEGIES: [&str; 4] = ["fetch", "decode", "reconstruct", "recompute"];

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.is_empty() {
        return f64::NAN;
    }
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let rem = secs % 86_400;
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{year:04}{m:02}{d:02}T{:02}{:02}{:02}{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        now.subsec_millis()
    )
}

fn content_root(bytes: &[u8]) -> Hash32 {
    identity::content_hash(bytes)
}

// ---------------------------------------------------------------------------
// the fixture: one logical tensor with four exact representations
// ---------------------------------------------------------------------------

struct Fixture {
    name: String,
    target: Vec<u8>,
    raw: Hash32,
    frame: Hash32,
    delta: Hash32,
    left: Hash32,
    right: Hash32,
    stored_bytes: [u64; 4],
}

struct Court {
    store: Store,
    fixtures: Vec<Fixture>,
}

fn build(corpus: &Path, work: &Path, max_tensors: usize, max_bytes: u64) -> Result<Court, String> {
    let store_dir = work.join("store");
    if store_dir.exists() {
        fs::remove_dir_all(&store_dir).map_err(|err| format!("clean: {err}"))?;
    }
    fs::create_dir_all(&store_dir).map_err(|err| format!("mkdir: {err}"))?;
    let mut store =
        Store::init(&store_dir, Options::default()).map_err(|err| format!("init: {err}"))?;

    let parent_path = corpus.join("Qwen2.5-0.5B.safetensors");
    let target_path = corpus.join("Qwen2.5-0.5B-Instruct.safetensors");
    let parent = SafetensorsFile::parse(&parent_path).map_err(|err| format!("parent: {err}"))?;
    let target = SafetensorsFile::parse(&target_path).map_err(|err| format!("target: {err}"))?;

    let mut candidates: Vec<_> = target
        .tensors
        .iter()
        .filter(|tensor| tensor.byte_len() >= 65536 && tensor.byte_len() <= max_bytes)
        .collect();
    candidates.sort_by_key(|tensor| (tensor.byte_len(), tensor.name.clone()));
    if candidates.len() > max_tensors {
        let stride = candidates.len() / max_tensors;
        candidates = candidates
            .into_iter()
            .enumerate()
            .filter(|(index, _)| index % stride.max(1) == 0)
            .map(|(_, tensor)| tensor)
            .take(max_tensors)
            .collect();
    }

    let mut fixtures = Vec::new();
    for tensor in candidates {
        let other = match parent.tensor(&tensor.name) {
            Ok(other) if other.shape == tensor.shape && other.dtype == tensor.dtype => other,
            _ => continue,
        };
        let bytes = target
            .read_tensor(tensor)
            .map_err(|err| format!("read: {err}"))?;
        let parent_bytes = parent
            .read_tensor(other)
            .map_err(|err| format!("read parent: {err}"))?;
        let layout = TensorLayoutSpec::row_major();

        // The parent's mode-9 object is written once per tensor name.
        let parent_name = format!("{}.parent", tensor.name);
        let parent_object = match store
            .write_tensor_file_with_spec(
                weftfs::state::State::ROOT_ID,
                parent_name.as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &parent_bytes,
                &layout,
                &TensorWriteSpec {
                    strategy_hint: Some(TensorStrategy::FloatTensor),
                    ..TensorWriteSpec::default()
                },
            ) {
            Ok(id) => id,
            Err(err) => {
                eprintln!("T096: skip {} (parent): {err}", tensor.name);
                continue;
            }
        };

        let raw_name = format!("{}.raw", tensor.name);
        let raw = store
            .create_file(
                weftfs::state::State::ROOT_ID,
                raw_name.as_bytes(),
                &bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("raw write: {err}"))?;

        let frame_name = format!("{}.frame", tensor.name);
        let frame = match store
            .write_tensor_file_with_spec(
                weftfs::state::State::ROOT_ID,
                frame_name.as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &TensorWriteSpec {
                    strategy_hint: Some(TensorStrategy::FloatTensor),
                    ..TensorWriteSpec::default()
                },
            ) {
            Ok(id) => id,
            Err(err) => {
                eprintln!("T096: skip {} (frame): {err}", tensor.name);
                continue;
            }
        };
        let parent_version = store
            .object_meta(parent_object)
            .map_err(|err| format!("parent meta: {err}"))?
            .version;
        let delta_name = format!("{}.delta", tensor.name);
        let delta = match store
            .write_tensor_file_with_spec(
                weftfs::state::State::ROOT_ID,
                delta_name.as_bytes(),
                &tensor.dtype,
                &tensor.shape,
                &bytes,
                &layout,
                &TensorWriteSpec {
                    strategy_hint: Some(TensorStrategy::WeightDelta),
                    causal: vec![parent_version],
                    ..TensorWriteSpec::default()
                },
            ) {
            Ok(id) => id,
            Err(err) => {
                eprintln!("T096: skip {} (delta): {err}", tensor.name);
                continue;
            }
        };

        // Two halves whose concatenation is the target, for the recompute path.
        let split = bytes.len() / 2;
        let (left_bytes, right_bytes) = bytes.split_at(split);
        let left = store
            .create_file(
                weftfs::state::State::ROOT_ID,
                format!("{}.left", tensor.name).as_bytes(),
                left_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("left write: {err}"))?;
        let right = store
            .create_file(
                weftfs::state::State::ROOT_ID,
                format!("{}.right", tensor.name).as_bytes(),
                right_bytes,
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .map_err(|err| format!("right write: {err}"))?;

        // The recompute contract: exact concatenation of two verified inputs.
        // The declared output root is the *object's* content root, which is
        // what the store publishes and what the planner verifies against.
        let frame_content = store
            .object_meta(frame)
            .map_err(|err| format!("frame meta: {err}"))?
            .content;
        let record = ProvenanceRecord {
            operation: "concat-halves".to_owned(),
            params: BTreeMap::new(),
            inputs: vec![
                ProvenanceInput::new("left", content_root(left_bytes), Some(left)),
                ProvenanceInput::new("right", content_root(right_bytes), Some(right)),
            ],
            outputs: vec![frame_content],
            environment: ProvenanceEnvironment::new("weftfs", "t096"),
            determinism: Determinism::Exact,
            principal: ProvenancePrincipal::new(0, 0),
            timestamp_ns: 1_700_000_000_000_000_000,
        };
        store
            .record_provenance(&[frame], &record)
            .map_err(|err| format!("provenance: {err}"))?;

        // The approximate candidate: declared determinism, an operation that
        // does not reproduce the target. It is attached to the same object so
        // that the planner sees it and must refuse it for an exact request.
        let approximate = ProvenanceRecord {
            operation: "concat-halves-truncated".to_owned(),
            params: BTreeMap::new(),
            inputs: vec![
                ProvenanceInput::new("left", content_root(left_bytes), Some(left)),
                ProvenanceInput::new("right", content_root(right_bytes), Some(right)),
            ],
            outputs: vec![frame_content],
            environment: ProvenanceEnvironment::new("weftfs", "t096"),
            determinism: Determinism::Declared,
            principal: ProvenancePrincipal::new(0, 0),
            timestamp_ns: 1_700_000_000_000_000_001,
        };
        store
            .record_provenance(&[frame], &approximate)
            .map_err(|err| format!("approximate provenance: {err}"))?;

        // Stored representation bytes, from the object manifests.
        let stored = |id: Hash32, store: &Store| -> u64 {
            store.object_meta(id).map(|meta| meta.size).unwrap_or(0)
        };
        let raw_stored = stored(raw, &store);
        let frame_stored = stored(frame, &store);
        let delta_stored = stored(delta, &store);
        let halves_stored = stored(left, &store) + stored(right, &store);

        fixtures.push(Fixture {
            name: tensor.name.clone(),
            target: bytes,
            raw,
            frame,
            delta,
            left,
            right,
            stored_bytes: [raw_stored, frame_stored, delta_stored, halves_stored],
        });
    }
    Ok(Court { store, fixtures })
}

// ---------------------------------------------------------------------------
// strategies
// ---------------------------------------------------------------------------

/// Execute one strategy and return `(wall_ns, output, exact)`.
fn execute(court: &Court, fixture: &Fixture, strategy: usize) -> Result<(f64, Vec<u8>), String> {
    let started = Instant::now();
    let out = match strategy {
        0 => court
            .store
            .read_object(fixture.raw)
            .map_err(|err| format!("fetch: {err}"))?,
        1 => court
            .store
            .read_tensor(fixture.frame)
            .map_err(|err| format!("decode: {err}"))?,
        2 => court
            .store
            .read_tensor(fixture.delta)
            .map_err(|err| format!("reconstruct: {err}"))?,
        3 => {
            let left = court
                .store
                .read_object(fixture.left)
                .map_err(|err| format!("recompute left: {err}"))?;
            let right = court
                .store
                .read_object(fixture.right)
                .map_err(|err| format!("recompute right: {err}"))?;
            let mut joined = Vec::with_capacity(left.len() + right.len());
            joined.extend_from_slice(&left);
            joined.extend_from_slice(&right);
            joined
        }
        _ => return Err("unknown strategy".to_owned()),
    };
    let wall = started.elapsed().as_secs_f64() * 1e9;
    Ok((wall, out))
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = PathBuf::from(
        arg(&args, "--corpus").unwrap_or_else(|| ".docker-cache/real-corpus".to_owned()),
    );
    let work = PathBuf::from(arg(&args, "--work").unwrap_or_else(|| "/tmp/t096".to_owned()));
    let out =
        PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "recompute-planner.json".to_owned()));
    let max_tensors: usize = arg(&args, "--tensors")
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    let max_bytes: u64 = arg(&args, "--max-bytes")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1024 * 1024);
    let reps: usize = arg(&args, "--reps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    fs::create_dir_all(&work).map_err(|err| format!("work dir: {err}"))?;

    let court = build(&corpus, &work, max_tensors, max_bytes)?;
    eprintln!(
        "T096: {} fixtures in {}",
        court.fixtures.len(),
        work.display()
    );

    // Availability masks. Bit i set = strategy i available. Training and
    // held-out masks are disjoint sets of patterns.
    let train_masks: [u8; 6] = [0b1111, 0b0011, 0b1100, 0b1010, 0b0101, 0b1110];
    let heldout_masks: [u8; 5] = [0b1001, 0b0110, 0b1101, 0b1011, 0b1000];

    let mut observations: BTreeMap<(usize, usize, u8), (f64, u64, bool)> = BTreeMap::new();
    let mut measured = 0usize;
    for (fixture_index, fixture) in court.fixtures.iter().enumerate() {
        for mask in train_masks.iter().chain(heldout_masks.iter()) {
            for strategy in 0..4 {
                if mask & (1 << strategy) == 0 {
                    continue;
                }
                let mut samples = Vec::with_capacity(reps);
                let mut exact = false;
                let mut bytes = 0u64;
                for _ in 0..reps {
                    let (wall, output) = execute(&court, fixture, strategy)?;
                    samples.push(wall);
                    bytes = output.len() as u64;
                    exact = output == fixture.target;
                }
                observations.insert(
                    (fixture_index, strategy, *mask),
                    (median(samples), bytes, exact),
                );
                measured += 1;
            }
        }
    }

    // Total observed cost per (request, strategy) and the oracle per request.
    let mut rows = Vec::new();
    let mut train_total = [0.0f64; 6];
    let mut heldout_oracle = 0.0f64;
    let mut heldout_fixed = [0.0f64; 6];
    let mut recompute_chosen = 0usize;
    let mut recompute_correct = 0usize;
    let mut approximate_refused = 0usize;
    let mut exact_requests = 0usize;

    // The shipped contract gate: build the candidate list once per fixture.
    let contracts: Vec<_> = court
        .fixtures
        .iter()
        .map(|fixture| {
            court
                .store
                .recovery_candidates(
                    fixture.frame,
                    RecoveryCosts {
                        fetch: 100,
                        decode: 1_000,
                        recompute: 5_000,
                    },
                )
                .map_err(|err| format!("candidates: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Mean observed cost per strategy on the training traces.
    let mut mean_train = [0.0f64; 4];
    let mut mean_count = [0usize; 4];
    for ((fixture_index, strategy, mask), (wall, _, _)) in observations.iter() {
        if !train_masks.contains(mask) {
            continue;
        }
        let _ = fixture_index;
        mean_train[*strategy] += wall;
        mean_count[*strategy] += 1;
    }
    for strategy in 0..4 {
        if mean_count[strategy] > 0 {
            mean_train[strategy] /= mean_count[strategy] as f64;
        } else {
            mean_train[strategy] = f64::INFINITY;
        }
    }

    // Best fixed priority order on the training traces (all 24 permutations).
    let permutations: Vec<[usize; 4]> = {
        let mut out = Vec::new();
        let mut indices = [0usize, 1, 2, 3];
        fn permute(items: &mut [usize; 4], start: usize, out: &mut Vec<[usize; 4]>) {
            if start == items.len() {
                out.push(*items);
                return;
            }
            for index in start..items.len() {
                items.swap(start, index);
                permute(items, start + 1, out);
                items.swap(start, index);
            }
        }
        permute(&mut indices, 0, &mut out);
        out
    };
    let mut best_order = permutations[0];
    let mut best_cost = f64::INFINITY;
    for order in &permutations {
        let mut total = 0.0;
        for (fixture_index, fixture) in court.fixtures.iter().enumerate() {
            for mask in train_masks {
                let choice = order
                    .iter()
                    .copied()
                    .find(|strategy| mask & (1 << strategy) != 0)
                    .unwrap_or(0);
                total += observations[&(fixture_index, choice, mask)].0;
            }
        }
        if total < best_cost {
            best_cost = total;
            best_order = *order;
        }
    }

    // Declared-cost policy: the shipped planner's ordering per fixture, filtered
    // by availability. `reconstruct` is priced between decode and recompute.
    let declared = [100.0f64, 1_000.0, 3_000.0, 5_000.0];

    for (fixture_index, fixture) in court.fixtures.iter().enumerate() {
        let candidates = &contracts[fixture_index];
        let exact_choice = choose_recovery(fixture.frame, candidates)
            .map_err(|err| format!("no eligible strategy: {err}"))?;
        // The shipped planner must never choose the approximate candidate.
        if exact_choice.name.contains("concat-halves-truncated") {
            return Err("the shipped planner chose an approximate candidate".to_owned());
        }
        exact_requests += 1;
        for mask in heldout_masks {
            let available: Vec<usize> = (0..4)
                .filter(|strategy| mask & (1 << *strategy) != 0)
                .collect();
            let oracle = available
                .iter()
                .map(|strategy| observations[&(fixture_index, *strategy, mask)].0)
                .fold(f64::INFINITY, f64::min);
            heldout_oracle += oracle;

            let fixed = best_order
                .iter()
                .copied()
                .find(|strategy| mask & (1 << strategy) != 0)
                .unwrap_or(0);
            let fixed_cost = observations[&(fixture_index, fixed, mask)].0;
            let mean_choice = available
                .iter()
                .copied()
                .min_by(|a, b| mean_train[*a].partial_cmp(&mean_train[*b]).unwrap())
                .unwrap_or(0);
            let mean_cost = observations[&(fixture_index, mean_choice, mask)].0;
            let declared_choice = available
                .iter()
                .copied()
                .min_by(|a, b| declared[*a].partial_cmp(&declared[*b]).unwrap())
                .unwrap_or(0);
            let declared_cost = observations[&(fixture_index, declared_choice, mask)].0;
            // The shipped planner's strategy, mapped onto the measured
            // strategies, then constrained to what this trace makes available.
            let preferred = match exact_choice.strategy {
                RecoveryStrategy::Fetch => 0,
                RecoveryStrategy::Decode => 1,
                RecoveryStrategy::Recompute => 3,
            };
            let shipped_choice = if available.contains(&preferred) {
                preferred
            } else {
                available[0]
            };
            let shipped_cost = observations[&(fixture_index, shipped_choice, mask)].0;

            heldout_fixed[0] += fixed_cost;
            heldout_fixed[1] += mean_cost;
            heldout_fixed[2] += declared_cost;
            heldout_fixed[3] += shipped_cost;

            // Count every policy's recompute decisions, not just the fixed
            // order: the fraction that were actually exact is the gate.
            for choice in [fixed, mean_choice, declared_choice, shipped_choice] {
                if choice == 3 {
                    recompute_chosen += 1;
                    if observations[&(fixture_index, 3, mask)].2 {
                        recompute_correct += 1;
                    }
                }
            }
            // The approximate candidate is present in every fixture; it is
            // "refused" when the shipped gate rejects it.
            if let Some(candidate) = contracts[fixture_index]
                .iter()
                .find(|candidate| candidate.determinism == Determinism::Declared)
            {
                if candidate.rejections().is_empty() {
                    return Err("the approximate candidate was eligible".to_owned());
                }
                approximate_refused += 1;
            }

            rows.push(json!({
                "fixture": fixture.name,
                "mask": mask,
                "available": available,
                "observed_ns": available.iter().map(|strategy| json!({
                    "strategy": STRATEGIES[*strategy],
                    "ns": observations[&(fixture_index, *strategy, mask)].0,
                    "bytes": observations[&(fixture_index, *strategy, mask)].1,
                    "exact": observations[&(fixture_index, *strategy, mask)].2,
                })).collect::<Vec<_>>(),
                "oracle_ns": oracle,
                "fixed_priority_ns": fixed_cost,
                "mean_utility_ns": mean_cost,
                "declared_cost_ns": declared_cost,
                "shipped_planner_ns": shipped_cost,
            }));
        }
    }

    let heldout_requests = rows.len() as f64;
    let oracle_cost = heldout_oracle;
    let policies = [
        ("best_fixed_priority", heldout_fixed[0]),
        ("mean_utility", heldout_fixed[1]),
        ("declared_cost", heldout_fixed[2]),
        ("shipped_planner", heldout_fixed[3]),
    ]
    .iter()
    .map(|(name, cost)| {
        json!({
            "policy": name,
            "cost_ns": cost,
            "regret_ns": cost - oracle_cost,
            "regret_fraction": (cost - oracle_cost) / oracle_cost,
            "mean_ns": cost / heldout_requests,
        })
    })
    .collect::<Vec<_>>();

    let document = json!({
        "schema": "weftfs.recompute-planner/1",
        "generated_at": utc_stamp(),
        "revision": std::env::var("WEFT_REPO_WEFTFS").unwrap_or_else(|_| "unknown".to_owned()),
        "environment": {
            "court": std::env::var("WEFT_COURT_IMAGE").unwrap_or_else(|_| "unknown".to_owned()),
            "kernel": std::env::var("WEFT_COURT_KERNEL").unwrap_or_else(|_| "unknown".to_owned()),
            "memory": std::env::var("WEFT_COURT_MEMORY").unwrap_or_else(|_| "?".to_owned()),
            "cpus": std::env::var("WEFT_COURT_CPUS").unwrap_or_else(|_| "?".to_owned()),
        },
        "configuration": {
            "corpus": corpus.display().to_string(),
            "fixtures": court.fixtures.len(),
            "max_tensor_bytes": max_bytes,
            "decode_reps": reps,
            "train_masks": train_masks.iter().map(|mask| format!("{mask:04b}")).collect::<Vec<_>>(),
            "heldout_masks": heldout_masks.iter().map(|mask| format!("{mask:04b}")).collect::<Vec<_>>(),
            "declared_costs": {"fetch": declared[0], "decode": declared[1],
                               "reconstruct": declared[2], "recompute": declared[3]},
            "best_fixed_order": best_order.iter().map(|s| STRATEGIES[*s]).collect::<Vec<_>>(),
            "mean_train_ns": STRATEGIES.iter().enumerate().map(|(index, name)| json!({
                "strategy": name, "mean_ns": mean_train[index]
            })).collect::<Vec<_>>(),
        },
        "stored_bytes": court.fixtures.iter().map(|fixture| json!({
            "name": fixture.name,
            "logical": fixture.target.len(),
            "fetch": fixture.stored_bytes[0],
            "decode": fixture.stored_bytes[1],
            "reconstruct": fixture.stored_bytes[2],
            "recompute_inputs": fixture.stored_bytes[3],
        })).collect::<Vec<_>>(),
        "contract": {
            "gate": "weftfs::provenance::choose_recovery (shipped)",
            "exact_requests": exact_requests,
            "recompute_chosen": recompute_chosen,
            "recompute_correct": recompute_correct,
            "recompute_correct_fraction": if recompute_chosen > 0 {
                recompute_correct as f64 / recompute_chosen as f64
            } else { 1.0 },
            "approximate_candidates": 1,
            "approximate_refusals_checked": approximate_refused,
            "approximate_selected": 0,
        },
        "heldout": {
            "requests": rows.len(),
            "observations": measured,
            "oracle_ns": oracle_cost,
            "policies": policies,
            "rows": rows,
        },
    });
    let bytes = serde_json::to_vec_pretty(&document).map_err(|err| err.to_string())?;
    fs::write(&out, bytes).map_err(|err| format!("write {}: {err}", out.display()))?;
    println!(
        "{}",
        json!({
            "out": out.display().to_string(),
            "fixtures": court.fixtures.len(),
            "requests": rows.len(),
            "oracle_ns": oracle_cost,
            "best_fixed": heldout_fixed[0],
            "mean_utility": heldout_fixed[1],
            "declared_cost": heldout_fixed[2],
            "shipped_planner": heldout_fixed[3],
            "recompute_correct_fraction": if recompute_chosen > 0 {
                recompute_correct as f64 / recompute_chosen as f64 } else { 1.0 },
        })
    );
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("recompute_planner_court: {err}");
        std::process::exit(1);
    }
}
