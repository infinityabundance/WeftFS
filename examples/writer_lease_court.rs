//! X09 court harness — writer session lease: expiry, renewal, fencing, fairness
//! and the cost the lease adds to a publication.
//!
//! Everything runs against one local store per case. The cases that need a
//! *process* fault (SIGKILL, SIGSTOP/SIGCONT) re-execute this same binary in a
//! child with the `child-hold` verb, which is a real second process rather than
//! a simulated one.
//!
//! Emits a JSON document on `--json <path>`; the shell court assembles the
//! receipt from it. Nothing here is a backend benchmark.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use weftfs::mw2::{TransactionOptions, WriterMode};
use weftfs::state::{
    derive_entry_id, derive_version_id, EntryRecord, ObjectKind, ObjectRecord, State,
};
use weftfs::writer_lease::{wall_now_ns, WriterLeaseOptions};
use weftfs::{OpenMode, Options, Store};

const NOW: i64 = 1_700_000_000_000_000_000;

fn options(mode: WriterMode) -> Options {
    Options {
        fixed_timestamp_ns: Some(NOW),
        writer_mode: mode,
        checkpoint_interval: 0,
        ..Options::default()
    }
}

fn lease(ttl: Duration, keeper: bool, fairness: bool, wait: Duration) -> Options {
    Options {
        writer_lease: Some(WriterLeaseOptions {
            ttl,
            keeper,
            fairness,
            ticket_ttl: wait + Duration::from_secs(2),
            wait,
        }),
        ..options(WriterMode::Optimistic)
    }
}

fn fresh_store(dir: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    {
        let store = Store::init(dir, options(WriterMode::Single)).unwrap();
        drop(store);
    }
    dir.to_path_buf()
}

fn open(path: &Path, options: Options) -> Store {
    Store::open_with_options(path, OpenMode::ReadWrite, options).unwrap()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn stats(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({
        "n": samples.len(),
        "min": samples.first().copied().unwrap_or(0.0),
        "p50": percentile(&samples, 0.50),
        "p95": percentile(&samples, 0.95),
        "p99": percentile(&samples, 0.99),
        "max": samples.last().copied().unwrap_or(0.0),
    })
}

struct Plan {
    record: ObjectRecord,
    entry: EntryRecord,
}

fn plan(store: &Store, name: &[u8], bytes: &[u8], seed: &[u8]) -> weftfs::error::Result<Plan> {
    let (manifest, _content, size) = store.prepare_content(bytes)?;
    let id = weftfs::identity::object_id_from_seed(seed);
    let version = derive_version_id(id, manifest, size, NOW, None)?;
    Ok(Plan {
        record: ObjectRecord {
            id,
            kind: ObjectKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            mtime_ns: NOW,
            version,
            content: manifest,
            size,
            attrs: BTreeMap::new(),
        },
        entry: EntryRecord {
            id: derive_entry_id(State::ROOT_ID, name)?,
            parent: State::ROOT_ID,
            name: name.to_vec(),
            target: id,
        },
    })
}

fn publish_one(
    store: &mut Store,
    name: &[u8],
    seed: &[u8],
    tx_id: &str,
) -> weftfs::error::Result<()> {
    let plan = plan(store, name, b"payload", seed)?;
    let mut tx = store.begin_transaction(tx_id)?;
    let state = store.current_state();
    tx.put_object(state, plan.record.clone())?;
    tx.put_entry(state, plan.entry.clone())?;
    tx.set_tip(state, plan.record.id, plan.record.version)?;
    store.execute_transaction(&tx, &TransactionOptions::default())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Child verb: hold a lease (optionally publishing once after a delay)
// ---------------------------------------------------------------------------

fn child_hold(args: &[String]) -> ! {
    // child-hold <path> <ttl-secs> <keeper:0|1> <hold-ms> <publish:0|1> <index>
    let path = PathBuf::from(&args[0]);
    let ttl = Duration::from_secs(args[1].parse().unwrap());
    let keeper = args[2] == "1";
    let hold_ms: u64 = args[3].parse().unwrap();
    let publish = args[4] == "1";
    let index: u32 = args[5].parse().unwrap();
    let mut store = match Store::open_with_options(
        &path,
        OpenMode::ReadWrite,
        lease(ttl, keeper, true, Duration::from_millis(200)),
    ) {
        Ok(store) => store,
        Err(err) => {
            println!(
                "{}",
                json!({"ok": false, "class": err.class_name(), "message": err.message()})
            );
            std::process::exit(3);
        }
    };
    let fence = store.writer_lease_view().map(|lease| lease.fence);
    println!(
        "{}",
        json!({"ok": true, "phase": "held", "fence": fence, "pid": std::process::id()})
    );
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::thread::sleep(Duration::from_millis(hold_ms));
    if publish {
        let name = format!("child-{index}");
        match publish_one(
            &mut store,
            name.as_bytes(),
            format!("seed-child-{index}").as_bytes(),
            &format!("0000000000000000000000000000{index:04x}"),
        ) {
            Ok(()) => {
                println!(
                    "{}",
                    json!({"ok": true, "phase": "published", "fence": store.writer_lease_view().map(|l| l.fence)})
                );
                std::process::exit(0);
            }
            Err(err) => {
                println!(
                    "{}",
                    json!({"ok": false, "phase": "publish", "class": err.class_name(), "message": err.message()})
                );
                std::process::exit(4);
            }
        }
    }
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

fn case_acquire_cost(root: &Path) -> serde_json::Value {
    let mut rows = Vec::new();
    for fairness in [true, false] {
        let path = fresh_store(&root.join(if fairness {
            "acquire-fair"
        } else {
            "acquire-race"
        }));
        let mut samples = Vec::new();
        for _ in 0..120 {
            let started = Instant::now();
            let store = open(
                &path,
                lease(ttl_duration(), true, fairness, Duration::from_millis(200)),
            );
            let acquired_us = started.elapsed().as_secs_f64() * 1e6;
            let fence = store
                .writer_lease_view()
                .map(|lease| lease.fence)
                .unwrap_or(0);
            // A graceful close writes the release tombstone; measure that too.
            let close_started = Instant::now();
            drop(store);
            let released_us = close_started.elapsed().as_secs_f64() * 1e6;
            let record = {
                let observer = Store::open_with_options(
                    &path,
                    OpenMode::ReadOnly,
                    options(WriterMode::Optimistic),
                )
                .unwrap();
                observer.writer_lease_record().unwrap()
            };
            assert!(record.is_some(), "the acquisition published a record");
            assert_eq!(record.unwrap().fence, fence);
            samples.push(acquired_us);
            let _ = released_us;
        }
        rows.push(json!({
            "fairness": fairness,
            "acquire_and_open_us": stats(samples),
        }));
    }
    json!({"rows": rows})
}

fn ttl_duration() -> Duration {
    Duration::from_secs(2)
}

fn case_idle_expiry(root: &Path) -> serde_json::Value {
    let path = fresh_store(&root.join("idle-expiry"));
    let holder = open(
        &path,
        lease(
            Duration::from_secs(2),
            false,
            true,
            Duration::from_millis(100),
        ),
    );
    let fence_holder = holder.writer_lease_view().unwrap().fence;
    let acquired_at = Instant::now();
    // The holder does nothing at all from here.
    let successor = open(
        &path,
        lease(Duration::from_secs(2), false, true, Duration::from_secs(6)),
    );
    let waited_ms = acquired_at.elapsed().as_secs_f64() * 1e3;
    let fence_successor = successor.writer_lease_view().unwrap().fence;
    let takeovers = successor.writer_lease_stats().unwrap().takeovers;
    json!({
        "fence_holder": fence_holder,
        "fence_successor": fence_successor,
        "takeovers": takeovers,
        "successor_wait_ms": waited_ms,
        "fence_incremented": fence_successor == fence_holder + 1,
        "successor_published": publish_one(&mut { successor }, b"post", b"seed-post", "000000000000000000000000000000aa").is_ok(),
    })
}

fn case_live_renewing(root: &Path) -> serde_json::Value {
    let path = fresh_store(&root.join("live-renewing"));
    let holder = open(
        &path,
        lease(
            Duration::from_secs(2),
            true,
            true,
            Duration::from_millis(100),
        ),
    );
    let fence_holder = holder.writer_lease_view().unwrap().fence;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut refusals = 0;
    let mut stolen = 0;
    while Instant::now() < deadline {
        match Store::open_with_options(
            &path,
            OpenMode::ReadWrite,
            lease(
                Duration::from_secs(2),
                false,
                true,
                Duration::from_millis(300),
            ),
        ) {
            Ok(_) => stolen += 1,
            Err(err) => {
                assert_eq!(err.class_name(), "Busy", "{err}");
                refusals += 1;
            }
        }
    }
    let renewals = holder.writer_lease_stats().unwrap().renewals;
    let record = holder.writer_lease_record().unwrap().unwrap();
    json!({
        "fence_holder": fence_holder,
        "fence_after": record.fence,
        "refusals": refusals,
        "stolen": stolen,
        "renewals": renewals,
        "remaining_ms": record.remaining_ms(wall_now_ns()),
    })
}

fn case_publication_cost(root: &Path) -> serde_json::Value {
    let mut rows = Vec::new();
    for leased in [false, true] {
        let path = fresh_store(&root.join(if leased {
            "publish-leased"
        } else {
            "publish-plain"
        }));
        let mut store = if leased {
            open(
                &path,
                lease(
                    Duration::from_secs(3),
                    true,
                    true,
                    Duration::from_millis(200),
                ),
            )
        } else {
            open(&path, options(WriterMode::Optimistic))
        };
        let mut samples = Vec::new();
        for index in 0..60u32 {
            let name = format!("p{index}");
            let started = Instant::now();
            publish_one(
                &mut store,
                name.as_bytes(),
                format!("seed-{index}").as_bytes(),
                &format!("{:032x}", 0xbb00u64 + u64::from(index)),
            )
            .unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1e3);
        }
        rows.push(json!({
            "lease": leased,
            "commit_ms": stats(samples),
            "generation": store.generation(),
            "verify_ok": store.verify(true).unwrap().ok,
        }));
    }
    json!({"rows": rows})
}

fn case_kill_child(root: &Path) -> serde_json::Value {
    let path = fresh_store(&root.join("kill-child"));
    let exe = std::env::current_exe().unwrap();
    let ttl = 2u64;
    let mut child = Command::new(&exe)
        .args([
            "child-hold",
            path.to_str().unwrap(),
            &ttl.to_string(),
            "0",
            "30000",
            "0",
            "1",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait for the "held" line.
    let mut line = String::new();
    use std::io::BufRead;
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    out.read_line(&mut line).unwrap();
    let held: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let fence_child = held["fence"].as_u64().unwrap();
    // Real SIGKILL: no Drop, no tombstone.
    let killed_at = Instant::now();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();
    let record = {
        let observer =
            Store::open_with_options(&path, OpenMode::ReadOnly, options(WriterMode::Optimistic))
                .unwrap();
        observer.writer_lease_record().unwrap().unwrap()
    };
    let state_after_kill = record.state.as_str().to_owned();
    let acquire = Store::open_with_options(
        &path,
        OpenMode::ReadWrite,
        lease(Duration::from_secs(2), false, true, Duration::from_secs(8)),
    );
    let waited_ms = killed_at.elapsed().as_secs_f64() * 1e3;
    let (ok, fence, class) = match acquire {
        Ok(store) => (
            true,
            store.writer_lease_view().map(|lease| lease.fence),
            None,
        ),
        Err(err) => (false, None, Some(err.class_name().to_owned())),
    };
    json!({
        "child_fence": fence_child,
        "record_state_after_kill": state_after_kill,
        "successor_acquired": ok,
        "successor_fence": fence,
        "successor_class": class,
        "waited_ms": waited_ms,
        "ttl_ms": ttl * 1000,
    })
}

fn case_stop_child(root: &Path) -> serde_json::Value {
    let path = fresh_store(&root.join("stop-child"));
    let exe = std::env::current_exe().unwrap();
    let ttl = 2u64;
    // The child publishes once after 5 s; we stop it before that, take over,
    // then let it resume and watch its publication be refused by the fence.
    let mut child = Command::new(&exe)
        .args([
            "child-hold",
            path.to_str().unwrap(),
            &ttl.to_string(),
            "1",
            "5000",
            "1",
            "2",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let mut out = std::io::BufReader::new(stdout);
    out.read_line(&mut line).unwrap();
    let held: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let fence_child = held["fence"].as_u64().unwrap();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGSTOP);
    }
    // Its keeper is stopped with it, so the lease expires.
    std::thread::sleep(Duration::from_millis(ttl * 1000 + 400));
    let mut successor = open(
        &path,
        lease(Duration::from_secs(2), false, true, Duration::from_secs(4)),
    );
    let fence_successor = successor.writer_lease_view().unwrap().fence;
    let published = publish_one(
        &mut successor,
        b"successor",
        b"seed-successor",
        "000000000000000000000000000000cc",
    )
    .is_ok();
    let head_after_successor = successor.head();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGCONT);
    }
    // The child resumes, sleeps out the rest of its 5 s, then publishes. Its
    // stdout was taken above, so the rest of the output is read from the same
    // reader, not from `wait_with_output` (which would see an empty pipe).
    let mut tail = String::new();
    {
        use std::io::Read;
        out.read_to_string(&mut tail).unwrap();
    }
    let status = child.wait().unwrap();
    let last: Option<serde_json::Value> = tail
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .filter(|value| value.get("phase").and_then(|p| p.as_str()) == Some("publish"))
        .next_back();
    let child_publish_ok = last
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(|ok| ok.as_bool())
        .unwrap_or(false);
    let child_class = last
        .as_ref()
        .and_then(|value| value.get("class"))
        .and_then(|class| class.as_str())
        .map(|class| class.to_owned());
    let observer =
        Store::open_with_options(&path, OpenMode::ReadOnly, options(WriterMode::Optimistic))
            .unwrap();
    let names: Vec<String> = observer
        .list_dir(State::ROOT_ID)
        .unwrap()
        .into_iter()
        .map(|entry| String::from_utf8_lossy(&entry.name).into_owned())
        .collect();
    json!({
        "child_fence": fence_child,
        "successor_fence": fence_successor,
        "fence_incremented": fence_successor == fence_child + 1,
        "successor_published": published,
        "child_publish_ok": child_publish_ok,
        "child_class": child_class,
        "child_exit_code": status.code(),
        "head_after_successor": head_after_successor.to_hex(),
        "head_now": observer.head().to_hex(),
        "head_unchanged_by_child": observer.head() == head_after_successor,
        "names": names,
        "verify_ok": observer.verify(true).unwrap().ok,
    })
}

fn case_fairness(root: &Path, fairness: bool) -> serde_json::Value {
    let path = fresh_store(&root.join(if fairness { "fair-queue" } else { "race-queue" }));
    let holder_path = path.clone();
    let holder = std::thread::spawn(move || {
        let store = open(
            &holder_path,
            lease(
                Duration::from_secs(2),
                true,
                fairness,
                Duration::from_millis(200),
            ),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        std::thread::sleep(Duration::from_millis(2500));
        fence
    });
    std::thread::sleep(Duration::from_millis(300));
    let waiter_path = path.clone();
    let waiter = std::thread::spawn(move || {
        let started = Instant::now();
        let store = open(
            &waiter_path,
            lease(
                Duration::from_secs(2),
                false,
                fairness,
                Duration::from_secs(10),
            ),
        );
        let fence = store.writer_lease_view().unwrap().fence;
        let waited = started.elapsed().as_secs_f64() * 1e3;
        std::thread::sleep(Duration::from_millis(300));
        drop(store);
        (fence, waited)
    });
    std::thread::sleep(Duration::from_millis(300));
    let mut newcomers_acquired = 0;
    let mut newcomers_refused = 0;
    let mut first_newcomer_ms: Option<f64> = None;
    let stream_started = Instant::now();
    for _ in 0..150u32 {
        let short = lease(
            Duration::from_secs(2),
            false,
            fairness,
            Duration::from_millis(40),
        );
        match Store::open_with_options(&path, OpenMode::ReadWrite, short) {
            Ok(store) => {
                newcomers_acquired += 1;
                if first_newcomer_ms.is_none() {
                    first_newcomer_ms = Some(stream_started.elapsed().as_secs_f64() * 1e3);
                }
                drop(store);
            }
            Err(err) => {
                assert_eq!(err.class_name(), "Busy", "{err}");
                newcomers_refused += 1;
            }
        }
    }
    let fence_holder = holder.join().unwrap();
    let (fence_waiter, waited_ms) = waiter.join().unwrap();
    json!({
        "fairness": fairness,
        "newcomers": 150,
        "newcomers_acquired_before_waiter": newcomers_acquired,
        "newcomers_refused": newcomers_refused,
        "first_newcomer_acquired_ms": first_newcomer_ms,
        "waiter_wait_ms": waited_ms,
        "fence_holder": fence_holder,
        "fence_waiter": fence_waiter,
        "waiter_got_next_fence": fence_waiter == fence_holder + 1,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|arg| arg.as_str()) == Some("child-hold") {
        child_hold(&args[1..]);
    }
    let mut out = PathBuf::from("/tmp/writer-lease-court");
    let mut json_path: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                out = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--json" => {
                json_path = Some(PathBuf::from(&args[index + 1]));
                index += 2;
            }
            other => panic!("unknown argument {other}"),
        }
    }
    std::fs::create_dir_all(&out).unwrap();

    let cases = [
        ("acquire_cost", case_acquire_cost(&out)),
        ("idle_expiry", case_idle_expiry(&out)),
        ("live_renewing", case_live_renewing(&out)),
        ("publication_cost", case_publication_cost(&out)),
        ("kill_child", case_kill_child(&out)),
        ("stop_child", case_stop_child(&out)),
        ("fairness_on", case_fairness(&out, true)),
        ("fairness_off", case_fairness(&out, false)),
    ];

    let mut checks = Vec::new();
    let mut check = |label: &str, ok: bool, detail: String| {
        checks.push(json!({"label": label, "ok": ok, "detail": detail}));
    };

    let by_name: BTreeMap<&str, &serde_json::Value> =
        cases.iter().map(|(name, value)| (*name, value)).collect();

    let idle = by_name["idle_expiry"];
    check(
        "idle_expiry: a takeover increments the fence",
        idle["fence_incremented"].as_bool().unwrap_or(false),
        format!(
            "holder {} -> successor {}",
            idle["fence_holder"], idle["fence_successor"]
        ),
    );
    check(
        "idle_expiry: the takeover waited about one lifetime",
        idle["successor_wait_ms"].as_f64().unwrap_or(0.0) >= 1500.0,
        format!("{} ms", idle["successor_wait_ms"]),
    );
    check(
        "idle_expiry: the successor publishes",
        idle["successor_published"].as_bool().unwrap_or(false),
        "transaction after takeover".to_owned(),
    );

    let live = by_name["live_renewing"];
    check(
        "live_renewing: a live renewing holder is never stolen",
        live["stolen"].as_u64() == Some(0) && live["refusals"].as_u64().unwrap_or(0) >= 5,
        format!(
            "{} refusals, {} steals, {} renewals",
            live["refusals"], live["stolen"], live["renewals"]
        ),
    );

    let kill = by_name["kill_child"];
    check(
        "kill_child: SIGKILL leaves no tombstone",
        kill["record_state_after_kill"].as_str() == Some("held"),
        format!("state {}", kill["record_state_after_kill"]),
    );
    check(
        "kill_child: the successor acquires after the deadline",
        kill["successor_acquired"].as_bool().unwrap_or(false)
            && kill["successor_fence"].as_u64() == kill["child_fence"].as_u64().map(|f| f + 1),
        format!(
            "waited {} ms of a {} ms lifetime",
            kill["waited_ms"], kill["ttl_ms"]
        ),
    );

    let stop = by_name["stop_child"];
    check(
        "stop_child: the successor takes over a stopped holder",
        stop["fence_incremented"].as_bool().unwrap_or(false)
            && stop["successor_published"].as_bool().unwrap_or(false),
        format!(
            "child fence {} -> successor {}",
            stop["child_fence"], stop["successor_fence"]
        ),
    );
    check(
        "stop_child: the resurrected stale writer is refused and mutates nothing",
        stop["child_publish_ok"].as_bool() == Some(false)
            && stop["child_class"].as_str() == Some("LeaseExpired")
            && stop["head_unchanged_by_child"].as_bool().unwrap_or(false),
        format!(
            "child class {:?}, exit {:?}, head unchanged {}",
            stop["child_class"], stop["child_exit_code"], stop["head_unchanged_by_child"]
        ),
    );
    check(
        "stop_child: only the successor's name is committed and the store verifies",
        stop["names"]
            .as_array()
            .map(|names| names.len() == 1 && names[0].as_str() == Some("successor"))
            .unwrap_or(false)
            && stop["verify_ok"].as_bool().unwrap_or(false),
        format!("names {}", stop["names"]),
    );

    let fair = by_name["fairness_on"];
    let race = by_name["fairness_off"];
    check(
        "fairness_on: no newcomer overtakes the queued waiter",
        fair["newcomers_acquired_before_waiter"].as_u64() == Some(0),
        format!(
            "{} acquired before the waiter, {} refused",
            fair["newcomers_acquired_before_waiter"], fair["newcomers_refused"]
        ),
    );
    check(
        "fairness_on: the waiter takes the next fence",
        fair["waiter_got_next_fence"].as_bool().unwrap_or(false),
        format!(
            "holder {} -> waiter {} after {} ms",
            fair["fence_holder"], fair["fence_waiter"], fair["waiter_wait_ms"]
        ),
    );
    check(
        "publication_cost: the leased path still verifies",
        by_name["publication_cost"]["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .all(|row| row["verify_ok"].as_bool() == Some(true))
            })
            .unwrap_or(false),
        "both arms".to_owned(),
    );
    check(
        "fairness_off: the control arm is reported, not asserted",
        race["newcomers"].as_u64() == Some(150),
        format!(
            "{} newcomer(s) acquired before the waiter, waiter waited {} ms",
            race["newcomers_acquired_before_waiter"], race["waiter_wait_ms"]
        ),
    );

    let document = json!({
        "schema": "weft.writer-lease.court.v1",
        "cases": cases.iter().map(|(name, value)| json!({"name": name, "result": value})).collect::<Vec<_>>(),
        "checks": checks,
        "checks_failed": checks.iter().filter(|check| check["ok"].as_bool() != Some(true)).count(),
        "environment": {
            "cpus": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(0),
            "target": std::env::consts::ARCH,
        },
    });
    let text = serde_json::to_string_pretty(&document).unwrap();
    match json_path {
        Some(path) => std::fs::write(&path, text).unwrap(),
        None => println!("{text}"),
    }
    if document["checks_failed"].as_u64().unwrap_or(1) != 0 {
        eprintln!(
            "writer_lease_court: {} check(s) failed",
            document["checks_failed"]
        );
        std::process::exit(1);
    }
}
