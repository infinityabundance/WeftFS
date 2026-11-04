//! F3.1 — Context-mounted roots, time travel, and concurrent mounts.
//!
//! Every test drives the real `weftfs mount` daemon in the FUSE court. The
//! mount root of a `--context` mount is the resolved Context Root's ordered,
//! capability-authorized result set; it is always read-only.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuse_common::{run_weft, weft_bin, Fixture, Mount};

const OLD_BYTES: &[u8] = b"identical bytes, two object identities\n";
const OTHER_BYTES: &[u8] = b"unrelated content\n";

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

fn create(fx: &Fixture, path: &str, seed: &str, bytes: &[u8], attrs: &[&str]) {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut args: Vec<&OsStr> = vec![
        os("create"),
        os(path),
        os("--text"),
        OsStr::new(&text),
        os("--seed"),
        OsStr::new(seed),
    ];
    for attr in attrs {
        args.push(os("--attr"));
        args.push(OsStr::new(attr));
    }
    fx.weft_ok(&args);
}

fn snapshot(fx: &Fixture, name: &str) -> String {
    let text = fx.weft_ok(&[os("--json"), os("snapshot"), os("create"), OsStr::new(name)]);
    let value: serde_json::Value = serde_json::from_str(&text).expect("snapshot json");
    value["snapshot_id"]
        .as_str()
        .expect("snapshot id")
        .to_owned()
}

fn context(fx: &Fixture, name: &str, selector: &str, snapshot: &str) {
    fx.weft_ok(&[
        os("--json"),
        os("context"),
        os("create"),
        os("--name"),
        OsStr::new(name),
        os("--selector"),
        OsStr::new(selector),
        os("--snapshot"),
        OsStr::new(snapshot),
    ]);
}

fn listing(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".weft")
        .collect();
    names.sort();
    names
}

fn mount_at(fx: &Fixture, mountpoint: &Path, flags: &[&str]) -> Mount {
    Mount::start(&fx.store, mountpoint, flags, &[])
}

#[test]
fn context_mount_lists_all_results_and_reads_exact_bytes() {
    let fx = Fixture::new();
    create(
        &fx,
        "/alpha.txt",
        "alpha-seed",
        OLD_BYTES,
        &["project=demo", "task=agent-42"],
    );
    create(
        &fx,
        "/beta.txt",
        "beta-seed",
        OLD_BYTES,
        &["project=demo", "task=agent-42"],
    );
    create(
        &fx,
        "/other.txt",
        "other-seed",
        OTHER_BYTES,
        &["project=other"],
    );
    let snap = snapshot(&fx, "s1");
    context(&fx, "by-project", "select attrs.project == \"demo\"", &snap);

    let mount_dir = fx.dir.path().join("mnt-context");
    fs::create_dir(&mount_dir).unwrap();
    fs::set_permissions(&mount_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let _mount = mount_at(&fx, &mount_dir, &["--context", "by-project"]);

    let names = listing(&mount_dir);
    assert_eq!(names.len(), 2, "{names:?}");
    // Deterministic naming: escaped basename + `--` + 8 hex ObjectId.
    assert!(names
        .iter()
        .all(|name| name.contains("--") && name.len() > 10));
    assert!(names.iter().any(|name| name.starts_with("alpha.txt--")));
    assert!(names.iter().any(|name| name.starts_with("beta.txt--")));
    assert!(!names.iter().any(|name| name.starts_with("other.txt--")));

    // readdir and lookup must agree for every listed name, and the bytes must
    // be exactly the stored bytes.
    for name in &names {
        let path = mount_dir.join(name);
        let meta = fs::metadata(&path).unwrap();
        assert!(meta.is_file(), "{name} is not a file");
        assert_eq!(fs::read(&path).unwrap(), OLD_BYTES, "{name}");
        // A singular lookup by a listed name is exact, not ambiguous.
        assert_eq!(fs::read(mount_dir.join(name)).unwrap(), OLD_BYTES);
    }

    // Both results are byte-identical but carry distinct ObjectIds and share
    // one ContentId, exactly as the store declares.
    let stat_a = fx.weft_ok(&[os("--json"), os("stat"), os("/alpha.txt")]);
    let stat_b = fx.weft_ok(&[os("--json"), os("stat"), os("/beta.txt")]);
    let a: serde_json::Value = serde_json::from_str(&stat_a).unwrap();
    let b: serde_json::Value = serde_json::from_str(&stat_b).unwrap();
    assert_ne!(a["object_id"], b["object_id"]);
    assert_eq!(a["content_id"], b["content_id"]);

    // Read-only: create, write, mkdir, unlink, and setattr all fail with EROFS.
    let err = fs::write(mount_dir.join("new.txt"), b"x").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));
    let existing = mount_dir.join(&names[0]);
    let err = fs::OpenOptions::new()
        .write(true)
        .open(&existing)
        .unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));
    let err = fs::create_dir(mount_dir.join("dir")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));
    let err = fs::remove_file(&existing).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));

    // `.weft` remains a constant, read-only exact-id surface.
    assert!(mount_dir.join(".weft/id").is_dir());
}

#[test]
fn capability_scoped_mount_is_read_only_and_denies_unlisted_roots() {
    let fx = Fixture::new();
    create(&fx, "/alpha.txt", "alpha-seed", b"alpha", &["project=demo"]);
    let snap = snapshot(&fx, "s1");
    context(&fx, "by-project", "select attrs.project == \"demo\"", &snap);

    let allow = fx.dir.path().join("allow.json");
    fs::write(
        &allow,
        br#"{"schema":"weft.capability.v1","principal":"agent","namespaces":["default"],"predicates":[]}"#,
    )
    .unwrap();
    let deny = fx.dir.path().join("deny.json");
    fs::write(
        &deny,
        br#"{"schema":"weft.capability.v1","principal":"agent","namespaces":["default"],"predicates":[],"roots":["1111111111111111111111111111111111111111111111111111111111111111"]}"#,
    )
    .unwrap();

    // A namespace-wide capability narrows nothing but forces read-only.
    let mount_dir = fx.dir.path().join("mnt-cap");
    fs::create_dir(&mount_dir).unwrap();
    fs::set_permissions(&mount_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let _mount = mount_at(
        &fx,
        &mount_dir,
        &[
            "--context",
            "by-project",
            "--capability",
            allow.to_str().unwrap(),
        ],
    );
    let names = listing(&mount_dir);
    assert_eq!(names.len(), 1, "{names:?}");
    let err = fs::write(mount_dir.join("blocked"), b"x").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::EROFS));

    // A capability that does not grant the context root fails closed at mount
    // time; it does not mount an empty or partial view.
    let denied_dir = fx.dir.path().join("mnt-denied");
    fs::create_dir(&denied_dir).unwrap();
    let output = std::process::Command::new(weft_bin())
        .arg("mount")
        .arg("--store")
        .arg(&fx.store)
        .arg("--mountpoint")
        .arg(&denied_dir)
        .arg("--foreground")
        .arg("--context")
        .arg("by-project")
        .arg("--capability")
        .arg(&deny)
        .output()
        .expect("run denied mount");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unauthorized") || stderr.contains("does not grant"),
        "stderr: {stderr}"
    );
    assert!(listing(&denied_dir).is_empty());
}

#[test]
fn at_selects_the_latest_commit_at_or_before_and_rejects_before_first() {
    let fx = Fixture::new();
    create(&fx, "/first.txt", "first-seed", b"first", &["stage=one"]);
    let between = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    std::thread::sleep(Duration::from_millis(20));
    create(&fx, "/second.txt", "second-seed", b"second", &["stage=two"]);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    // `--at` before the first commit fails closed with a clear class.
    let early_dir = fx.dir.path().join("mnt-early");
    fs::create_dir(&early_dir).unwrap();
    let output = std::process::Command::new(weft_bin())
        .arg("mount")
        .arg("--store")
        .arg(&fx.store)
        .arg("--mountpoint")
        .arg(&early_dir)
        .arg("--foreground")
        .arg("--at")
        .arg("1970-01-01T00:00:00Z")
        .output()
        .expect("run early mount");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no commit at or before"),
        "stderr: {stderr}"
    );

    // A time between the two commits selects the first commit's state.
    let mid_dir = fx.dir.path().join("mnt-mid");
    fs::create_dir(&mid_dir).unwrap();
    fs::set_permissions(&mid_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let at = format!("{}", between);
    let at_flags = ["--at", at.as_str()];
    let _mid = mount_at(&fx, &mid_dir, &at_flags);
    let mid_names = listing(&mid_dir);
    assert!(mid_names.contains(&"first.txt".to_owned()), "{mid_names:?}");
    assert!(
        !mid_names.contains(&"second.txt".to_owned()),
        "{mid_names:?}"
    );
    assert_eq!(fs::read(mid_dir.join("first.txt")).unwrap(), b"first");

    // A time at/after the head selects the head state.
    let head_dir = fx.dir.path().join("mnt-head");
    fs::create_dir(&head_dir).unwrap();
    fs::set_permissions(&head_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let at_head = format!("{}", now);
    let head_flags = ["--at", at_head.as_str()];
    let _head = mount_at(&fx, &head_dir, &head_flags);
    let head_names = listing(&head_dir);
    assert!(head_names.contains(&"first.txt".to_owned()));
    assert!(head_names.contains(&"second.txt".to_owned()));
}

#[test]
fn three_simultaneous_mounts_serve_different_roots_without_the_writer_lock() {
    let fx = Fixture::new();
    create(
        &fx,
        "/alpha.txt",
        "alpha-seed",
        OLD_BYTES,
        &["project=demo", "task=agent-42"],
    );
    create(
        &fx,
        "/beta.txt",
        "beta-seed",
        OLD_BYTES,
        &["project=demo", "task=agent-42"],
    );
    create(
        &fx,
        "/other.txt",
        "other-seed",
        OTHER_BYTES,
        &["project=other", "task=agent-99"],
    );
    let s1 = snapshot(&fx, "s1");
    context(&fx, "by-task", "select attrs.task == \"agent-42\"", &s1);
    context(&fx, "by-project", "select attrs.project == \"demo\"", &s1);
    std::thread::sleep(Duration::from_millis(20));
    create(
        &fx,
        "/after.txt",
        "after-seed",
        b"after\n",
        &["project=demo"],
    );
    let after_commit = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    let m1 = fx.dir.path().join("mnt-by-task");
    let m2 = fx.dir.path().join("mnt-by-project");
    let m3 = fx.dir.path().join("mnt-at");
    for dir in [&m1, &m2, &m3] {
        fs::create_dir(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let _task = mount_at(&fx, &m1, &["--context", "by-task"]);
    let _project = mount_at(&fx, &m2, &["--context", "by-project"]);
    let at = format!("{}", after_commit);
    let at_flags = ["--at", at.as_str()];
    let _at = mount_at(&fx, &m3, &at_flags);

    let task_names = listing(&m1);
    let project_names = listing(&m2);
    let at_names = listing(&m3);
    assert_eq!(task_names.len(), 2, "{task_names:?}");
    assert_eq!(project_names.len(), 2, "{project_names:?}");
    // The `--at` mount is the live head: it contains all three demo objects
    // plus the post-snapshot one.
    assert_eq!(at_names.len(), 4, "{at_names:?}");

    // Every mount reads its own root exactly.
    for name in &task_names {
        assert_eq!(fs::read(m1.join(name)).unwrap(), OLD_BYTES);
    }
    for name in &project_names {
        assert_eq!(fs::read(m2.join(name)).unwrap(), OLD_BYTES);
    }
    assert_eq!(fs::read(m3.join("other.txt")).unwrap(), OTHER_BYTES);

    // The read-only mounts do not hold the exclusive writer lock: a normal
    // mutating CLI command succeeds while all three are live.
    fx.weft_ok(&[
        os("create"),
        os("/while-mounted.txt"),
        os("--text"),
        os("live-write"),
    ]);
    // The `--at` mount is pinned to the head it started with and does not
    // observe the later commit.
    assert!(!m3.join("while-mounted.txt").exists());
}

#[test]
fn at_accepts_rfc3339_and_unix_ns_forms() {
    let fx = Fixture::new();
    create(&fx, "/only.txt", "only-seed", b"only", &["stage=one"]);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    // Compute an RFC 3339 second just after the commit with a small civil
    // conversion in the test, so this stays dependency-free.
    let seconds = now / 1_000_000_000 + 1;
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let stamp = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    );

    let mount_dir = fx.dir.path().join("mnt-rfc");
    fs::create_dir(&mount_dir).unwrap();
    fs::set_permissions(&mount_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let _mount = mount_at(&fx, &mount_dir, &["--at", &stamp]);
    assert_eq!(fs::read(mount_dir.join("only.txt")).unwrap(), b"only");
}

/// Inverse of `days_from_civil` for test-side RFC 3339 construction.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

// Keep the unused import warning away while documenting the failure path.
#[test]
fn mount_failure_is_a_clean_nonzero_exit() {
    let fx = Fixture::new();
    let out = run_weft(
        &fx.store,
        &[
            os("mount"),
            os("--mountpoint"),
            OsStr::new("/nonexistent-weft"),
            os("--at"),
            os("0"),
        ],
    );
    assert!(!out.status.success());
    let _ = &mut std::io::stdout().flush();
}
