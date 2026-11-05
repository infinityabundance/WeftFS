//! Q3 court: quota usage is carried by the commit itself.
//!
//! These tests prove that usage is part of committed state (`set_quota_usage`
//! op), survives reopen without a per-commit quota file write, is validated on
//! replay, is atomic with group commits, and that a legacy combined record is
//! imported once and preserved as `quota.migrated`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;
use weftcodec::wcb::Value;

use weftfs::error::Class;
use weftfs::format::decode_commit_file;
use weftfs::quota::{
    QuotaBudget, QuotaCounters, QuotaPolicyRecord, QuotaPrincipal, QuotaRecord, QuotaUsageOp,
    QUOTA_MAGIC, QUOTA_POLICY_MAGIC,
};
use weftfs::state::{Op, ATTR_TENANT};
use weftfs::store::{OpenMode, Options, Store};

const TS: i64 = 1_700_000_000_000_000_000;

fn options(chunk_size: u64, quotas: Vec<QuotaBudget>) -> Options {
    Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(TS),
        quotas,
        ..Options::default()
    }
}

fn posix(uid: u64, gid: u64, bytes: u64, objects: u64) -> QuotaBudget {
    QuotaBudget {
        principal: QuotaPrincipal::Posix { uid, gid },
        max_new_bytes: bytes,
        max_objects: objects,
    }
}

fn principal() -> QuotaPrincipal {
    QuotaPrincipal::Posix { uid: 0, gid: 0 }
}

fn create(store: &mut Store, name: &str, body: &[u8]) -> weftfs::identity::Hash32 {
    let root = store.resolve_path("/").unwrap();
    store
        .create_file(root, name.as_bytes(), body, 0o644, 0, 0, BTreeMap::new())
        .unwrap()
}

fn head_ops(dir: &Path, generation: u64) -> Vec<Op> {
    let path = dir.join("commits").join(format!("{generation}.commit"));
    let bytes = fs::read(path).unwrap();
    let (_, commit) = decode_commit_file(&bytes).unwrap();
    commit.ops
}

#[test]
fn usage_is_a_committed_op_and_the_quota_file_is_policy_only() {
    let dir = tempdir().unwrap();
    let opts = options(1024, vec![posix(0, 0, 1 << 20, 100)]);
    let mut store = Store::init(dir.path(), opts.clone()).unwrap();
    create(&mut store, "a.txt", b"alpha");
    create(&mut store, "b.txt", b"beta-beta");

    let usage = store.quota_usage(&principal());
    assert_eq!(usage.used_objects, 2);
    let generation = store.generation();
    let ops = head_ops(dir.path(), generation);
    let declared = match ops.last() {
        Some(Op::SetQuotaUsage(value)) => value.clone(),
        other => panic!("head commit does not end with a usage op: {other:?}"),
    };
    assert!(!declared.reset);
    assert_eq!(
        declared
            .usage
            .iter()
            .find(|(p, _)| *p == principal())
            .map(|(_, c)| *c),
        Some(usage)
    );

    // The persisted `quota` file is policy-only and does not carry usage.
    let policy_bytes = fs::read(dir.path().join("quota")).unwrap();
    assert!(policy_bytes.starts_with(QUOTA_POLICY_MAGIC));
    assert!(!policy_bytes.starts_with(QUOTA_MAGIC));
    assert!(!dir.path().join("quota.migrated").exists());

    // Reopen without restating the policy: usage comes from the commit log.
    drop(store);
    let store = Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![]))
        .expect("reopen");
    assert!(store.quota_enforced());
    assert_eq!(store.quota_usage(&principal()), usage);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn budget_denial_in_a_group_is_atomic_and_charges_nothing() {
    let dir = tempdir().unwrap();
    let mut store =
        Store::init(dir.path(), options(1024, vec![posix(0, 0, 64 * 1024, 2)])).unwrap();
    create(&mut store, "first.txt", b"first");
    let before = store.quota_usage(&principal());
    let generation = store.generation();
    let head = store.head();

    let outcome = store.group(|store| {
        create(store, "second.txt", b"second");
        create(store, "third.txt", b"third");
        // The third object exceeds max_objects = 2.
        create(store, "fourth.txt", b"fourth");
        Ok(())
    });
    assert_eq!(outcome.unwrap_err().class(), Class::BudgetExceeded);
    assert_eq!(store.generation(), generation);
    assert_eq!(store.head(), head);
    assert_eq!(store.quota_usage(&principal()), before);
    assert!(store.verify(true).unwrap().ok);

    // A group that fits is one commit and one usage op.
    let ((), hash) = store
        .group(|store| {
            create(store, "second.txt", b"second");
            Ok(())
        })
        .unwrap();
    assert_eq!(store.head(), hash);
    assert_eq!(store.generation(), generation + 1);
    let ops = head_ops(dir.path(), store.generation());
    assert!(ops.len() >= 3, "create ops plus the usage op: {ops:?}");
    assert!(matches!(ops.last(), Some(Op::SetQuotaUsage(_))));
    assert_eq!(store.quota_usage(&principal()).used_objects, 2);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn tampered_usage_assertion_is_corrupt_on_replay() {
    let dir = tempdir().unwrap();
    let mut store =
        Store::init(dir.path(), options(1024, vec![posix(0, 0, 1 << 20, 100)])).unwrap();
    create(&mut store, "a.txt", b"alpha");
    let usage = store.quota_usage(&principal());

    // Publish a syntactically valid but inconsistent assertion. The commit
    // itself is durable and hash-addressed; replay must still reject it.
    let mut wrong = usage;
    wrong.used_bytes = wrong.used_bytes.saturating_add(1);
    store
        .publish_quota_usage_for_test(QuotaUsageOp {
            reset: false,
            usage: vec![(principal(), wrong)],
        })
        .unwrap();

    let report = store.verify(true).unwrap();
    assert!(!report.ok, "full verify must reject the assertion");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.detail.contains("quota")),
        "problems: {:?}",
        report.problems
    );
    assert_eq!(store.quota_rebuild().unwrap_err().class(), Class::Corrupt);
}

#[test]
fn usage_survives_reopen_without_a_per_commit_file_write() {
    let dir = tempdir().unwrap();
    let mut store =
        Store::init(dir.path(), options(1024, vec![posix(0, 0, 1 << 20, 100)])).unwrap();
    create(&mut store, "a.txt", b"alpha");
    let policy_before = fs::read(dir.path().join("quota")).unwrap();
    let usage_before = store.quota_usage(&principal());

    for index in 0..20u32 {
        create(&mut store, &format!("f{index}.bin"), b"payload");
    }
    let policy_after = fs::read(dir.path().join("quota")).unwrap();
    assert_eq!(
        policy_before, policy_after,
        "the policy file must not be rewritten per commit"
    );
    let usage_after = store.quota_usage(&principal());
    assert_eq!(usage_after.used_objects, 21);
    assert!(usage_after.used_bytes > usage_before.used_bytes);
    drop(store);

    let store =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![])).unwrap();
    assert_eq!(store.quota_usage(&principal()), usage_after);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn usage_is_derived_when_the_head_has_no_assertion() {
    let dir = tempdir().unwrap();
    // Build a store with no policy, so its chain has no usage ops.
    let mut store = Store::init(dir.path(), options(1024, vec![])).unwrap();
    create(&mut store, "a.txt", b"alpha");
    create(&mut store, "b.txt", b"beta");
    let expected = store.quota_usage(&principal()); // empty without a policy
    assert_eq!(expected.used_objects, 0);
    drop(store);

    // Install a policy-only file directly and reopen. Usage must be derived
    // from the published chain and validated, not read from a record.
    let policy = QuotaPolicyRecord::new(vec![posix(0, 0, 1 << 20, 100)])
        .unwrap()
        .encode_bytes()
        .unwrap();
    fs::write(dir.path().join("quota"), &policy).unwrap();
    let store =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![])).unwrap();
    assert!(store.quota_enforced());
    let derived = store.quota_usage(&principal());
    assert_eq!(derived.used_objects, 2);
    assert!(derived.used_bytes > 0);
    assert!(store.verify(true).unwrap().ok);
}

#[test]
fn legacy_record_is_imported_once_and_preserved() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(dir.path(), options(1024, vec![])).unwrap();
    create(&mut store, "a.txt", b"alpha");
    let generation = store.generation();
    let head = store.head();
    drop(store);

    // A legacy combined record. It names an older generation, so migration
    // recomputes usage from the authoritative chain instead of trusting it.
    let legacy = QuotaRecord {
        generation: generation.saturating_sub(1),
        commit: head,
        budgets: vec![posix(0, 0, 1 << 20, 100)],
        usage: vec![(principal(), QuotaCounters::default())],
    }
    .normalize()
    .unwrap()
    .encode_bytes()
    .unwrap();
    fs::write(dir.path().join("quota"), &legacy).unwrap();

    let mut reopened =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![]))
            .expect("open migrates");
    assert!(reopened.quota_enforced());
    assert!(reopened.quota_problem().is_none());
    let usage = reopened.quota_usage(&principal());
    assert_eq!(usage.used_objects, 1, "usage recomputed from the chain");

    let policy_bytes = fs::read(dir.path().join("quota")).unwrap();
    assert!(policy_bytes.starts_with(QUOTA_POLICY_MAGIC));
    let preserved = fs::read(dir.path().join("quota.migrated")).unwrap();
    assert_eq!(preserved, legacy, "legacy bytes are preserved verbatim");

    // One further write still works and extends the committed usage.
    create(&mut reopened, "b.txt", b"beta");
    assert_eq!(reopened.quota_usage(&principal()).used_objects, 2);
    assert!(reopened.verify(true).unwrap().ok);
    drop(reopened);

    // A second open is idempotent: no second migration record is written.
    let again =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![])).unwrap();
    assert_eq!(again.quota_usage(&principal()).used_objects, 2);
    assert!(again.verify(true).unwrap().ok);
}

#[test]
fn migrated_record_that_disagrees_with_committed_usage_fails_closed() {
    let dir = tempdir().unwrap();
    let mut store =
        Store::init(dir.path(), options(1024, vec![posix(0, 0, 1 << 20, 100)])).unwrap();
    create(&mut store, "a.txt", b"alpha");
    let generation = store.generation();
    let head = store.head();
    let usage = store.quota_usage(&principal());
    drop(store);

    // Preserve a legacy record that names the current head but disagrees.
    let wrong = QuotaRecord {
        generation,
        commit: head,
        budgets: vec![posix(0, 0, 1 << 20, 100)],
        usage: vec![(
            principal(),
            QuotaCounters {
                used_bytes: usage.used_bytes + 1,
                used_objects: usage.used_objects,
            },
        )],
    }
    .normalize()
    .unwrap()
    .encode_bytes()
    .unwrap();
    fs::write(dir.path().join("quota.migrated"), &wrong).unwrap();

    let mut reopened =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![]))
            .expect("open succeeds; writes fail closed");
    assert!(
        reopened.quota_problem().is_some(),
        "disagreement must be reported"
    );
    let generation_before = reopened.generation();
    let err = reopened
        .create_file(
            reopened.resolve_path("/").unwrap(),
            b"blocked.txt",
            b"blocked",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap_err();
    assert_eq!(err.class(), Class::Corrupt);
    assert_eq!(reopened.generation(), generation_before);
}

#[test]
fn tenant_and_usage_op_bytes_are_canonical() {
    let op = QuotaUsageOp {
        reset: false,
        usage: vec![
            (
                QuotaPrincipal::Tenant("zeta".to_owned()),
                QuotaCounters {
                    used_bytes: 5,
                    used_objects: 1,
                },
            ),
            (
                principal(),
                QuotaCounters {
                    used_bytes: 10,
                    used_objects: 2,
                },
            ),
        ],
    }
    .normalize()
    .unwrap();
    let value = op.to_value();
    let encoded = weftfs::wcb_util::encode(&value).unwrap();
    let decoded = weftfs::wcb_util::decode(&encoded).unwrap();
    let map = weftfs::wcb_util::as_map(&decoded).unwrap();
    assert_eq!(
        weftfs::wcb_util::get_text(map, "op").unwrap(),
        "set_quota_usage"
    );
    assert_eq!(
        weftfs::wcb_util::field(map, "reset").unwrap(),
        &Value::Bool(false)
    );
    // Parsing the op from WCB must reproduce the canonical order.
    let parsed = Op::from_wcb(&decoded).unwrap();
    assert_eq!(
        parsed,
        Op::SetQuotaUsage(QuotaUsageOp {
            reset: false,
            usage: vec![
                (
                    principal(),
                    QuotaCounters {
                        used_bytes: 10,
                        used_objects: 2
                    }
                ),
                (
                    QuotaPrincipal::Tenant("zeta".to_owned()),
                    QuotaCounters {
                        used_bytes: 5,
                        used_objects: 1
                    }
                ),
            ],
        })
    );
}

#[test]
fn tenant_usage_is_committed_and_replayed() {
    let dir = tempdir().unwrap();
    let mut store = Store::init(
        dir.path(),
        options(
            1024,
            vec![
                posix(0, 0, 1 << 20, 100),
                QuotaBudget {
                    principal: QuotaPrincipal::Tenant("acme".to_owned()),
                    max_new_bytes: 1 << 20,
                    max_objects: 100,
                },
            ],
        ),
    )
    .unwrap();
    let root = store.resolve_path("/").unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert(ATTR_TENANT.to_owned(), Value::Str("acme".to_owned()));
    store
        .create_file(root, b"tenant.txt", b"tenant body", 0o644, 0, 0, attrs)
        .unwrap();
    let tenant = QuotaPrincipal::Tenant("acme".to_owned());
    let usage = store.quota_usage(&tenant);
    assert_eq!(usage.used_objects, 1);
    drop(store);

    let store =
        Store::open_with_options(dir.path(), OpenMode::ReadWrite, options(1024, vec![])).unwrap();
    assert_eq!(store.quota_usage(&tenant), usage);
    assert!(store.verify(true).unwrap().ok);
}
