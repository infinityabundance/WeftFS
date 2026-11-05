//! Write-budget courts (P5a, part 1).
//!
//! These tests prove admission-time enforcement, non-disclosure, reuse
//! accounting, reopen durability, fail-closed behaviour on a mismatched
//! record, group-commit rollback, and the tenant form of the attribution rule.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value as Json;
use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::error::Class;
use weftfs::quota::{parse_budget, parse_quota_file, QuotaBudget, QuotaPrincipal, QuotaRecord};
use weftfs::state::ATTR_TENANT;
use weftfs::store::{OpenMode, Options, Store};

const TS: i64 = 1_700_000_000_000_000_000;

fn options(chunk_size: u64) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(TS),
        ..Options::default()
    }
}

fn varied(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xff) as u8
        })
        .collect()
}

fn posix(uid: u64, gid: u64, bytes: u64, objects: u64) -> QuotaBudget {
    QuotaBudget {
        principal: QuotaPrincipal::Posix { uid, gid },
        max_new_bytes: bytes,
        max_objects: objects,
    }
}

fn tenant(name: &str, bytes: u64, objects: u64) -> QuotaBudget {
    QuotaBudget {
        principal: QuotaPrincipal::Tenant(name.to_owned()),
        max_new_bytes: bytes,
        max_objects: objects,
    }
}

#[test]
fn over_budget_write_is_refused_and_nothing_is_published() {
    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![posix(0, 0, 8_192, 100)];
    let mut store = Store::init(dir.path(), opts).unwrap();
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, b"small.txt", b"hello", 0o644, 0, 0, BTreeMap::new())
        .expect("small write is inside the budget");

    let generation = store.generation();
    let head = store.head();
    let objects = store.current_state().objects.len();
    let entries = store.current_state().entries.len();
    let usage_before = store.quota_usage(&QuotaPrincipal::Posix { uid: 0, gid: 0 });
    assert!(usage_before.used_bytes > 0);
    assert_eq!(usage_before.used_objects, 1);

    let big = vec![0x5au8; 512 * 1024];
    let err = store
        .create_file(root, b"big.bin", &big, 0o644, 0, 0, BTreeMap::new())
        .expect_err("512 KiB cannot fit an 8 KiB budget");
    assert_eq!(err.class(), Class::BudgetExceeded);
    // Non-disclosing: fixed text, no usage, no principal, no limit.
    assert_eq!(err.message(), "write budget exceeded");

    assert_eq!(store.generation(), generation, "generation must not move");
    assert_eq!(store.head(), head, "head must not move");
    assert_eq!(store.current_state().objects.len(), objects);
    assert_eq!(store.current_state().entries.len(), entries);
    assert_eq!(
        store.quota_usage(&QuotaPrincipal::Posix { uid: 0, gid: 0 }),
        usage_before,
        "a denied commit must not be charged"
    );
    assert!(store.verify(true).expect("verify").ok);
}

#[test]
fn reused_content_costs_only_the_new_object_record() {
    let dir = tempdir().unwrap();
    let mut opts = options(4096);
    opts.quotas = vec![posix(0, 0, u64::MAX, u64::MAX)];
    let mut store = Store::init(dir.path(), opts).unwrap();
    let root = store.resolve_path("/").unwrap();
    let principal = QuotaPrincipal::Posix { uid: 0, gid: 0 };
    let body = varied(64 * 1024);

    store
        .create_file(root, b"first.bin", &body, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let after_first = store.quota_usage(&principal);
    store.mkdir(root, b"dir", 0o755, 0, 0).unwrap();
    let after_dir = store.quota_usage(&principal);
    store
        .create_file(
            store.resolve_path("/dir").unwrap(),
            b"second.bin",
            &body,
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    let after_second = store.quota_usage(&principal);

    // Same content: identical chunks and identical manifest, so only the new
    // object record (plus entry-free record bytes) is charged.
    let delta = after_second.used_bytes - after_dir.used_bytes;
    assert!(
        delta > 0 && delta < body.len() as u64,
        "identical content must not be charged again (delta={delta})"
    );
    assert_eq!(after_second.used_objects - after_dir.used_objects, 1);
    // The first write paid for the content; a directory record is cheap.
    assert!(after_first.used_bytes >= body.len() as u64);
    assert!(after_dir.used_bytes - after_first.used_bytes < 1024);
}

#[test]
fn usage_survives_reopen_and_still_denies() {
    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![posix(0, 0, 16 * 1024, 3)];
    let principal = QuotaPrincipal::Posix { uid: 0, gid: 0 };
    let mut store = Store::init(dir.path(), opts.clone()).unwrap();
    let root = store.resolve_path("/").unwrap();
    for index in 0..2u32 {
        store
            .create_file(
                root,
                format!("f{index}").as_bytes(),
                format!("body {index}").as_bytes(),
                0o644,
                0,
                0,
                BTreeMap::new(),
            )
            .unwrap();
    }
    let before = store.quota_usage(&principal);
    drop(store);

    // Reopen *without* restating the policy: the persisted record enforces it.
    let mut store =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024)).expect("reopen");
    assert!(store.quota_enforced());
    assert_eq!(store.quota_usage(&principal), before);
    let third = store
        .create_file(root, b"f2", b"body 2", 0o644, 0, 0, BTreeMap::new())
        .expect("third object is inside max_objects=3");
    assert_ne!(third, weftfs::identity::Hash32::ZERO);
    let err = store
        .create_file(root, b"f3", b"body 3", 0o644, 0, 0, BTreeMap::new())
        .expect_err("max_objects=3 is exhausted");
    assert_eq!(err.class(), Class::BudgetExceeded);
    let after = store.quota_usage(&principal);
    assert_eq!(after.used_objects, 3);
    assert!(after.used_bytes >= before.used_bytes);
}

#[test]
fn mismatched_record_fails_closed_until_rebuild() {
    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![posix(0, 0, 64 * 1024, 10)];
    let mut store = Store::init(dir.path(), opts.clone()).unwrap();
    store
        .create_file(
            store.resolve_path("/").unwrap(),
            b"one.txt",
            b"one",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    drop(store);

    // Tamper with the persisted record: the digest no longer matches.
    let quota_path = dir.path().join("quota");
    let mut bytes = fs::read(&quota_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&quota_path, &bytes).unwrap();

    let mut store = Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024))
        .expect("open succeeds; only writes fail closed");
    assert!(store.quota_problem().is_some());
    let generation = store.generation();
    let err = store
        .create_file(
            store.resolve_path("/").unwrap(),
            b"two.txt",
            b"two",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect_err("a tampered record must not be trusted");
    assert_eq!(err.class(), Class::Corrupt);
    assert_eq!(store.generation(), generation);

    // Rebuilding without a policy would silently disable enforcement, so it is
    // refused; the explicit admin path is to re-supply the budgets.
    assert_eq!(store.quota_rebuild().unwrap_err().class(), Class::Corrupt);
    store
        .install_quota_policy(vec![posix(0, 0, 64 * 1024, 10)])
        .expect("install quota policy replays usage and repairs the record");
    assert!(store.quota_problem().is_none());
    store
        .create_file(
            store.resolve_path("/").unwrap(),
            b"two.txt",
            b"two",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .expect("write accepted after rebuild");
}

#[test]
fn tenant_budget_is_attributed_by_the_reserved_attribute() {
    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![tenant("acme", u64::MAX, 1)];
    let mut store = Store::init(dir.path(), opts).unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert(ATTR_TENANT.to_owned(), Value::Str("acme".to_owned()));

    store
        .create_file(root, b"a.txt", b"a", 0o644, 0, 0, attrs.clone())
        .unwrap();
    assert_eq!(
        store
            .quota_usage(&QuotaPrincipal::Tenant("acme".to_owned()))
            .used_objects,
        1
    );
    let err = store
        .create_file(root, b"b.txt", b"b", 0o644, 0, 0, attrs)
        .expect_err("tenant object budget is exhausted");
    assert_eq!(err.class(), Class::BudgetExceeded);
    // A file without the tenant attribute is not charged to the tenant.
    store
        .create_file(root, b"c.txt", b"c", 0o644, 0, 0, BTreeMap::new())
        .expect("untagged write");
    // POSIX usage is separate and never disclosed through the tenant view.
    assert_eq!(
        store
            .quota_usage(&QuotaPrincipal::Posix { uid: 0, gid: 0 })
            .used_objects,
        2
    );
}

#[test]
fn group_commit_denial_rolls_back_the_whole_group() {
    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![posix(0, 0, 8 * 1024, 100)];
    let mut store = Store::init(dir.path(), opts).unwrap();
    let root = store.resolve_path("/").unwrap();
    let generation = store.generation();
    let head = store.head();
    let outcome = store.group(|inner| {
        inner.create_file(
            inner.resolve_path("/").unwrap(),
            b"fits.txt",
            b"small",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )?;
        inner.create_file(
            root,
            b"too-big.bin",
            &varied(64 * 1024),
            0o644,
            0,
            0,
            BTreeMap::new(),
        )?;
        Ok(())
    });
    let err = outcome.err().expect("the group must be refused");
    assert_eq!(err.class(), Class::BudgetExceeded);
    assert_eq!(store.generation(), generation);
    assert_eq!(store.head(), head);
    assert!(store.resolve_path("/fits.txt").is_err());
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn quota_record_round_trips_canonically() {
    let record = QuotaRecord {
        generation: 7,
        commit: weftfs::identity::h("weft/test/v1", b"9"),
        budgets: vec![tenant("zeta", 5, 6), posix(42, 7, 1, 2), posix(0, 0, 3, 4)],
        usage: vec![
            (
                QuotaPrincipal::Tenant("zeta".to_owned()),
                Default::default(),
            ),
            (
                QuotaPrincipal::Posix { uid: 0, gid: 0 },
                weftfs::quota::QuotaCounters {
                    used_bytes: 10,
                    used_objects: 1,
                },
            ),
        ],
    }
    .normalize()
    .unwrap();
    let bytes = record.encode_bytes().unwrap();
    let decoded = QuotaRecord::decode_bytes(&bytes).unwrap();
    assert_eq!(decoded, record);
    assert_eq!(
        decoded.budgets[0].principal,
        QuotaPrincipal::Posix { uid: 0, gid: 0 }
    );
    assert_eq!(
        decoded.budgets[1].principal,
        QuotaPrincipal::Posix { uid: 42, gid: 7 }
    );
    assert_eq!(
        decoded.budgets[2].principal,
        QuotaPrincipal::Tenant("zeta".to_owned())
    );
    // A single flipped byte in the payload is a hard failure.
    let mut corrupt = bytes.clone();
    corrupt[20] ^= 0x01;
    assert_eq!(
        QuotaRecord::decode_bytes(&corrupt).unwrap_err().class(),
        Class::Corrupt
    );
}

#[test]
fn quota_arguments_parse_and_reject_ambiguity() {
    let budget = parse_budget("1000:1000:1048576:16").unwrap();
    assert_eq!(
        budget.principal,
        QuotaPrincipal::Posix {
            uid: 1000,
            gid: 1000
        }
    );
    assert_eq!(budget.max_new_bytes, 1_048_576);
    assert_eq!(budget.max_objects, 16);
    let budget = parse_budget("tenant:acme:100:2").unwrap();
    assert_eq!(budget.principal, QuotaPrincipal::Tenant("acme".to_owned()));
    assert!(parse_budget("nonsense").is_err());
    assert!(parse_budget("a:b:c:d").is_err());

    let file = parse_quota_file("# comment\n\n0:0:10:1\ntenant:acme:20:2\n").unwrap();
    assert_eq!(file.len(), 2);

    let dir = tempdir().unwrap();
    let mut opts = options(1024);
    opts.quotas = vec![posix(0, 0, 1, 1), posix(0, 0, 2, 2)];
    assert!(Store::init(dir.path(), opts).is_err());
}

// ---------------------------------------------------------------------------
// CLI surface: --quota, quota show (own usage only), reset/rebuild
// ---------------------------------------------------------------------------

fn run(store: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_weftfs"));
    command.arg("--store").arg(store);
    command.args(args);
    command.output().expect("run weftfs")
}

fn ok(store: &Path, args: &[&str]) -> Output {
    let output = run(store, args);
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(store: &Path, args: &[&str]) -> Json {
    let output = ok(store, args);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid JSON from {args:?}: {err}\nstdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn cli_quota_show_is_limited_to_the_callers_principal() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(
        &store,
        &[
            "--quota",
            "0:0:1048576:100",
            "--quota",
            "1000:1000:1:1",
            "init",
        ],
    );
    ok(&store, &["create", "/a.txt", "--text", "alpha"]);

    let own = json(
        &store,
        &["--json", "quota", "show", "--uid", "0", "--gid", "0"],
    );
    assert_eq!(own["principal"], "0:0");
    assert_eq!(own["configured"], true);
    assert!(own["used_bytes"].as_u64().unwrap() > 0);
    assert_eq!(own["used_objects"], 1);

    // The restricted principal's view contains only its own zero counters and
    // its own limits; the other principal's usage is not present.
    let other = json(
        &store,
        &["--json", "quota", "show", "--uid", "1000", "--gid", "1000"],
    );
    assert_eq!(other["principal"], "1000:1000");
    assert_eq!(other["used_bytes"], 0);
    assert_eq!(other["used_objects"], 0);
    assert_eq!(other["max_new_bytes"], 1);
    let text = other.to_string();
    assert!(!text.contains("\"0:0\""));

    // A write that would cross the restricted budget fails with the fixed,
    // non-disclosing message and does not publish.
    let body = dir.path().join("big.bin");
    fs::write(&body, vec![0x44u8; 32 * 1024]).unwrap();
    let output = run(
        &store,
        &[
            "create",
            "/restricted.bin",
            "--uid",
            "1000",
            "--gid",
            "1000",
            "--file",
            body.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("write budget exceeded"));

    // Reset is an explicit admin operation and is visible to the next show.
    ok(&store, &["quota", "reset"]);
    let reset = json(
        &store,
        &["--json", "quota", "show", "--uid", "0", "--gid", "0"],
    );
    assert_eq!(reset["used_bytes"], 0);
    assert_eq!(reset["used_objects"], 0);
    // The reset is itself a committed usage baseline, so rebuilding from the
    // chain respects it instead of resurrecting pre-reset usage.
    ok(&store, &["quota", "rebuild"]);
    let rebuilt = json(
        &store,
        &["--json", "quota", "show", "--uid", "0", "--gid", "0"],
    );
    assert_eq!(rebuilt["used_objects"], 0);
    ok(&store, &["create", "/after-reset.txt", "--text", "after"]);
    let after_reset = json(
        &store,
        &["--json", "quota", "show", "--uid", "0", "--gid", "0"],
    );
    assert_eq!(after_reset["used_objects"], 1);
}

#[test]
fn cli_quota_denial_is_non_disclosing() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("store");
    ok(&store, &["--quota", "0:0:4096:100", "init"]);
    let body = dir.path().join("big.bin");
    fs::write(&body, vec![0x44u8; 256 * 1024]).unwrap();
    let output = run(
        &store,
        &["create", "/big.bin", "--file", body.to_str().unwrap()],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("BudgetExceeded"), "stderr: {stderr}");
    assert!(stderr.contains("write budget exceeded"), "stderr: {stderr}");
    // Nothing about the current usage, the limit, or the principal.
    for forbidden in ["used", "4096", "max", "uid", "1000"] {
        assert!(
            !stderr.contains(forbidden),
            "denial leaked `{forbidden}`: {stderr}"
        );
    }
    let info = json(&store, &["--json", "info"]);
    assert_eq!(info["objects"], 1, "only the namespace root exists");
}
