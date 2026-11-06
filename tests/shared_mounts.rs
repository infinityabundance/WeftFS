//! R7 — one process serving N read-only context mounts over one shared store.
//!
//! Every test drives the real `weftfs mount-many` daemon in the FUSE court and
//! compares it against the equivalent single-mount daemons (`weftfs mount`).
//! The correctness bar is exact equivalence on the read surface: the same
//! listing digests, the same file bytes, the same capability denials, the same
//! snapshot pinning. Writes must fail closed with `EROFS`.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fuse_common::{is_mounted, weft_bin, Fixture, Mount};
use sha2::{Digest, Sha256};

const TS1: &str = "1767225600000000000";
const TS2: &str = "1767225660000000000";

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn create(fx: &Fixture, path: &str, text: &str, attrs: &[&str], ts: &str) {
    let mut args: Vec<&OsStr> = vec![
        os("--fixed-timestamp-ns"),
        OsStr::new(ts),
        os("create"),
        OsStr::new(path),
        os("--text"),
        OsStr::new(text),
        os("--seed"),
        OsStr::new(path),
    ];
    for attr in attrs {
        args.push(os("--attr"));
        args.push(OsStr::new(attr));
    }
    fx.weft_ok(&args);
}

fn snapshot(fx: &Fixture, name: &str) {
    fx.weft_ok(&[
        os("--fixed-timestamp-ns"),
        os(TS1),
        os("snapshot"),
        os("create"),
        os(name),
    ]);
}

fn context(fx: &Fixture, name: &str, selector: &str, snapshot: &str) -> String {
    let text = fx.weft_ok(&[
        os("--json"),
        os("--fixed-timestamp-ns"),
        os(TS1),
        os("context"),
        os("create"),
        os("--name"),
        OsStr::new(name),
        os("--selector"),
        OsStr::new(selector),
        os("--snapshot"),
        OsStr::new(snapshot),
    ]);
    let value: serde_json::Value = serde_json::from_str(&text).expect("context json");
    value["context_root_id"]
        .as_str()
        .expect("context root id")
        .to_owned()
}

fn object_id(fx: &Fixture, path: &str) -> String {
    let text = fx.weft_ok(&[
        os("--json"),
        os("--fixed-timestamp-ns"),
        os(TS1),
        os("stat"),
        OsStr::new(path),
    ]);
    let value: serde_json::Value = serde_json::from_str(&text).expect("stat json");
    value["object_id"].as_str().expect("object id").to_owned()
}

/// Shared fixture: a small namespace with two contexts, a snapshot, and a
/// later commit that only the live view sees.
fn build_store() -> Fixture {
    let fx = Fixture::new();
    fx.weft_ok(&[os("--fixed-timestamp-ns"), os(TS1), os("mkdir"), os("/sub")]);
    create(&fx, "/a.txt", "alpha\n", &["task=t1", "model=m1"], TS1);
    create(&fx, "/b.txt", "beta\n", &["task=t2", "model=m2"], TS1);
    create(&fx, "/sub/c.txt", "gamma\n", &["task=t1"], TS1);
    snapshot(&fx, "s1");
    create(&fx, "/late.txt", "late\n", &["task=t1"], TS2);
    context(&fx, "by:t1", "select attrs.task == \"t1\"", "s1");
    context(&fx, "by:t2", "select attrs.task == \"t2\"", "s1");
    fx
}

#[derive(Clone, Debug)]
enum ViewKind {
    Context(String),
    Snapshot(String),
    Head,
}

impl ViewKind {
    fn spec_fields(&self) -> Vec<(&'static str, String)> {
        match self {
            ViewKind::Context(name) => vec![("context", name.clone())],
            ViewKind::Snapshot(name) => vec![("snapshot", name.clone())],
            ViewKind::Head => Vec::new(),
        }
    }

    fn single_flags(&self) -> Vec<String> {
        match self {
            ViewKind::Context(name) => vec!["--context".to_owned(), name.clone()],
            ViewKind::Snapshot(name) => vec!["--snapshot".to_owned(), name.clone()],
            ViewKind::Head => vec!["--read-only".to_owned()],
        }
    }
}

struct SharedServer {
    child: Child,
    mountpoints: Vec<PathBuf>,
    stderr: PathBuf,
    stopped: bool,
}

impl SharedServer {
    fn start(fx: &Fixture, views: &[(ViewKind, Option<String>)], dirs: &[PathBuf]) -> SharedServer {
        assert!(
            Path::new("/dev/fuse").exists(),
            "/dev/fuse is unavailable; this test requires the fuse court"
        );
        for dir in dirs {
            fs::create_dir_all(dir).expect("create mountpoint");
        }
        let spec_path = fx.dir.path().join("mount-many.json");
        let views_json: Vec<serde_json::Value> = views
            .iter()
            .zip(dirs)
            .map(|((kind, capability), dir)| {
                let mut object = serde_json::Map::new();
                object.insert(
                    "mountpoint".to_owned(),
                    serde_json::Value::String(dir.to_string_lossy().into_owned()),
                );
                for (key, value) in kind.spec_fields() {
                    object.insert(key.to_owned(), serde_json::Value::String(value));
                }
                if let Some(capability) = capability {
                    object.insert(
                        "capability".to_owned(),
                        serde_json::Value::String(capability.clone()),
                    );
                }
                serde_json::Value::Object(object)
            })
            .collect();
        let spec = serde_json::json!({"version": 1, "views": views_json});
        fs::write(&spec_path, serde_json::to_string(&spec).unwrap()).expect("write spec");
        let stderr = fx.dir.path().join("mount-many.err");
        let stdout = fx.dir.path().join("mount-many.out");
        let child = Command::new(weft_bin())
            .arg("--store")
            .arg(&fx.store)
            .arg("mount-many")
            .arg("--spec")
            .arg(&spec_path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
            .spawn()
            .expect("spawn weftfs mount-many");
        let mut server = SharedServer {
            child,
            mountpoints: dirs.to_vec(),
            stderr,
            stopped: false,
        };
        server.await_ready();
        server
    }

    fn await_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "mount-many exited early ({status}); stderr:\n{}",
                    self.stderr_text()
                );
            }
            if self.mountpoints.iter().all(|dir| is_mounted(dir)) {
                std::thread::sleep(Duration::from_millis(50));
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "not every shared view became a mount; stderr:\n{}",
            self.stderr_text()
        );
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stderr_text(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn thread_count(&self) -> u64 {
        let status = fs::read_to_string(format!("/proc/{}/status", self.pid())).unwrap_or_default();
        status
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    }

    fn unmount(&mut self, index: usize) {
        let dir = &self.mountpoints[index];
        if !is_mounted(dir) {
            return;
        }
        let status = Command::new("fusermount3")
            .arg("-u")
            .arg(dir)
            .status()
            .expect("run fusermount3");
        assert!(status.success(), "fusermount3 -u {} failed", dir.display());
        let deadline = Instant::now() + Duration::from_secs(10);
        while is_mounted(dir) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!is_mounted(dir), "{} did not unmount", dir.display());
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // SIGTERM triggers the supervisor's clean unmount of every view.
        unsafe {
            libc::kill(self.pid() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        for dir in &self.mountpoints {
            if is_mounted(dir) {
                let _ = Command::new("umount").arg("-l").arg(dir).status();
            }
        }
        self.stopped = true;
    }
}

impl Drop for SharedServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Sorted `type ./path` lines for every entry below `dir`, excluding the
/// `/.weft` virtual surface. Two equivalent mounts must produce identical text.
fn listing(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("read_dir {}: {err}", dir.display()))
            .map(|entry| entry.expect("dir entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if dir == base && name == ".weft" {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(base)
                .expect("entry below base")
                .to_string_lossy()
                .into_owned();
            let meta = fs::symlink_metadata(entry.path()).expect("symlink_metadata");
            let kind = if meta.file_type().is_symlink() {
                'l'
            } else if meta.is_dir() {
                'd'
            } else {
                'f'
            };
            out.push(format!("{kind} ./{rel}"));
            if meta.is_dir() {
                walk(base, &entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn tree_digest(dir: &Path) -> String {
    sha256(format!("{}\n", listing(dir).join("\n")).as_bytes())
}

fn file_bytes(dir: &Path) -> BTreeMap<String, String> {
    listing(dir)
        .into_iter()
        .filter(|line| line.starts_with('f'))
        .map(|line| {
            let rel = line.strip_prefix("f ./").expect("file line").to_owned();
            let bytes = fs::read(dir.join(&rel)).expect("read mounted file");
            (rel, sha256(&bytes))
        })
        .collect()
}

fn errno(err: std::io::Error) -> Option<i32> {
    err.raw_os_error()
}

#[test]
fn shared_views_match_equivalent_single_mount_daemons() {
    let fx = build_store();
    let kinds = vec![
        ViewKind::Context("by:t1".to_owned()),
        ViewKind::Context("by:t2".to_owned()),
        ViewKind::Snapshot("s1".to_owned()),
        ViewKind::Head,
        ViewKind::Context("by:t1".to_owned()),
        ViewKind::Snapshot("s1".to_owned()),
        ViewKind::Head,
        ViewKind::Context("by:t2".to_owned()),
    ];
    let views: Vec<(ViewKind, Option<String>)> =
        kinds.iter().cloned().map(|kind| (kind, None)).collect();
    let shared_dirs: Vec<PathBuf> = (0..kinds.len())
        .map(|i| fx.dir.path().join(format!("shared-{i}")))
        .collect();
    let mut server = SharedServer::start(&fx, &views, &shared_dirs);
    let threads = server.thread_count() as usize;
    assert!(
        threads >= kinds.len() && threads <= kinds.len() + 3,
        "expected one session thread per view, measured {threads} for {} views",
        kinds.len()
    );

    let mut singles: Vec<Mount> = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        let dir = fx.dir.path().join(format!("single-{i}"));
        fs::create_dir(&dir).expect("create single mountpoint");
        let flags: Vec<String> = kind.single_flags();
        let borrow: Vec<&str> = flags.iter().map(String::as_str).collect();
        singles.push(Mount::start(&fx.store, &dir, &borrow, &[]));
    }

    for i in 0..kinds.len() {
        assert_eq!(
            tree_digest(&shared_dirs[i]),
            tree_digest(&singles[i].mountpoint),
            "view {i} ({:?}) listing differs from the equivalent single mount",
            kinds[i]
        );
        assert_eq!(
            file_bytes(&shared_dirs[i]),
            file_bytes(&singles[i].mountpoint),
            "view {i} ({:?}) file bytes differ from the equivalent single mount",
            kinds[i]
        );
    }
    // The pinned snapshot must not show the later commit, and the contexts
    // must remain distinct from the live view.
    assert!(!listing(&shared_dirs[2])
        .iter()
        .any(|line| line.contains("late.txt")));
    assert!(listing(&shared_dirs[3])
        .iter()
        .any(|line| line.contains("late.txt")));
    assert_ne!(tree_digest(&shared_dirs[0]), tree_digest(&shared_dirs[1]));
    assert_ne!(tree_digest(&shared_dirs[3]), tree_digest(&shared_dirs[2]));
    assert!(server.alive(), "shared server exited during the comparison");
    server.stop();
}

#[test]
fn shared_view_refuses_write_like_operations_with_erofs() {
    let fx = build_store();
    let dirs = vec![fx.dir.path().join("w0")];
    let views = vec![(ViewKind::Head, None)];
    let mut server = SharedServer::start(&fx, &views, &dirs);
    let mount = &dirs[0];
    let before = tree_digest(mount);

    let existing = fs::read_dir(mount)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .find(|name| name != ".weft")
        .expect("a visible entry");
    let existing = mount.join(existing);

    let attempts: Vec<(&str, std::io::Error)> = vec![
        (
            "create",
            fs::write(mount.join("created.txt"), b"x").unwrap_err(),
        ),
        (
            "mkdir",
            fs::create_dir(mount.join("created-dir")).unwrap_err(),
        ),
        (
            "open-write",
            fs::OpenOptions::new()
                .write(true)
                .open(&existing)
                .unwrap_err(),
        ),
        ("unlink", fs::remove_file(&existing).unwrap_err()),
        (
            "rename",
            fs::rename(&existing, mount.join("renamed")).unwrap_err(),
        ),
        (
            "chmod",
            fs::set_permissions(&existing, fs::Permissions::from_mode(0o600)).unwrap_err(),
        ),
        (
            "symlink",
            std::os::unix::fs::symlink("target", mount.join("link")).unwrap_err(),
        ),
        (
            "link",
            fs::hard_link(&existing, mount.join("hard")).unwrap_err(),
        ),
        (
            "truncate",
            fs::OpenOptions::new()
                .truncate(true)
                .write(true)
                .open(&existing)
                .unwrap_err(),
        ),
    ];
    for (name, err) in attempts {
        assert_eq!(
            errno(err),
            Some(libc::EROFS),
            "{name} through a shared view did not fail with EROFS"
        );
    }
    assert_eq!(
        tree_digest(mount),
        before,
        "a refused write changed the view"
    );
    assert!(server.alive(), "shared server exited after refusing writes");
    server.stop();
}

#[test]
fn shared_capability_scoped_view_cannot_see_an_unauthorized_object() {
    let fx = Fixture::new();
    create(&fx, "/public.txt", "public\n", &["task=public"], TS1);
    create(&fx, "/secret.txt", "secret\n", &["task=secret"], TS1);
    snapshot(&fx, "s1");
    let public_context = context(&fx, "by:public", "select attrs.task == \"public\"", "s1");
    context(&fx, "by:secret", "select attrs.task == \"secret\"", "s1");
    let public_id = object_id(&fx, "/public.txt");
    let secret_id = object_id(&fx, "/secret.txt");

    // Grant exactly one Context Root and no namespace.
    let capability = fx.dir.path().join("cap.json");
    fs::write(
        &capability,
        format!(
            "{{\"schema\":\"weft.capability.v1\",\"principal\":\"court\",\"namespaces\":[],\"roots\":[\"{public_context}\"],\"predicates\":[\"tagged_as\"]}}"
        ),
    )
    .unwrap();

    let dirs = vec![fx.dir.path().join("cap-0"), fx.dir.path().join("cap-1")];
    let views = vec![
        (
            ViewKind::Context("by:public".to_owned()),
            Some(capability.to_string_lossy().into_owned()),
        ),
        (ViewKind::Context("by:secret".to_owned()), None),
    ];
    let mut server = SharedServer::start(&fx, &views, &dirs);

    // The scoped view resolves, but only through the granted root.
    let granted = fs::read(dirs[0].join(format!(".weft/id/{public_id}"))).expect("granted id");
    assert_eq!(granted, b"public\n");
    let denied = fs::read(dirs[0].join(format!(".weft/id/{secret_id}")))
        .expect_err("unauthorized exact id must not be readable");
    assert_eq!(errno(denied), Some(libc::ENOENT));

    // The unscoped sibling view can read the secret through its own exact id.
    let visible = fs::read(dirs[1].join(format!(".weft/id/{secret_id}"))).expect("secret id");
    assert_eq!(visible, b"secret\n");

    // Capability roots never widen to the other context's result set.
    let scoped_listing = listing(&dirs[0]);
    assert!(scoped_listing
        .iter()
        .any(|line| line.contains("public.txt")));
    assert!(!scoped_listing
        .iter()
        .any(|line| line.contains("secret.txt")));
    assert!(
        server.alive(),
        "shared server exited during capability checks"
    );
    server.stop();
}

#[test]
fn unmounting_one_shared_view_leaves_the_others_serving() {
    let fx = build_store();
    let dirs: Vec<PathBuf> = (0..3)
        .map(|i| fx.dir.path().join(format!("u{i}")))
        .collect();
    let views = vec![
        (ViewKind::Head, None),
        (ViewKind::Snapshot("s1".to_owned()), None),
        (ViewKind::Context("by:t1".to_owned()), None),
    ];
    let mut server = SharedServer::start(&fx, &views, &dirs);
    let before: Vec<String> = dirs.iter().map(|dir| tree_digest(dir)).collect();

    server.unmount(0);
    assert!(!is_mounted(&dirs[0]));
    assert!(server.alive(), "unmounting one view ended the server");
    for index in 1..3 {
        assert_eq!(
            tree_digest(&dirs[index]),
            before[index],
            "view {index} changed after view 0 was unmounted"
        );
        assert!(
            !file_bytes(&dirs[index]).is_empty(),
            "view {index} stopped serving reads after view 0 was unmounted"
        );
    }

    // Unmount the rest: with no views left, the supervisor exits by itself.
    server.unmount(1);
    server.unmount(2);
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !server.alive(),
        "server survived after every view was unmounted"
    );
}

#[test]
fn shared_view_thread_count_is_bounded_and_oversized_specs_are_refused() {
    let fx = build_store();
    let dirs: Vec<PathBuf> = (0..8)
        .map(|i| fx.dir.path().join(format!("t{i}")))
        .collect();
    let views: Vec<(ViewKind, Option<String>)> = (0..8).map(|_| (ViewKind::Head, None)).collect();
    let mut server = SharedServer::start(&fx, &views, &dirs);
    let threads = server.thread_count();
    assert!(
        threads >= 8 && threads <= 8 + 3,
        "expected one session thread per view (8..11), measured {threads}"
    );
    server.stop();

    // MAX_SHARED_VIEWS + 1 views must be refused before anything is mounted.
    let limit = weftfs::fuse::MAX_SHARED_VIEWS;
    assert_eq!(limit, 256);
    let spec_path = fx.dir.path().join("too-many.json");
    let views_json: Vec<serde_json::Value> = (0..=limit)
        .map(|i| {
            serde_json::json!({
                "mountpoint": fx.dir.path().join(format!("never-{i}")).to_string_lossy()
            })
        })
        .collect();
    fs::write(
        &spec_path,
        serde_json::to_string(&serde_json::json!({"version": 1, "views": views_json})).unwrap(),
    )
    .unwrap();
    let output = Command::new(weft_bin())
        .arg("--store")
        .arg(&fx.store)
        .arg("mount-many")
        .arg("--spec")
        .arg(&spec_path)
        .output()
        .expect("run mount-many");
    assert!(!output.status.success(), "oversized spec was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LimitExceeded") && stderr.contains(&limit.to_string()),
        "unexpected refusal: {stderr}"
    );
}

#[test]
fn no_panic_escapes_a_shared_view() {
    let fx = build_store();
    let dirs = vec![fx.dir.path().join("panic-0")];
    let views = vec![(ViewKind::Head, None)];
    let mut server = SharedServer::start(&fx, &views, &dirs);
    let mount = &dirs[0];

    // A mix of valid and invalid operations that reach the daemon.
    let mut noise: Vec<std::io::Error> = Vec::new();
    noise.push(fs::read(mount.join("missing.txt")).unwrap_err());
    noise.push(fs::read_dir(mount.join("a.txt")).unwrap_err());
    noise.push(fs::create_dir(mount.join("d")).unwrap_err());
    noise.push(fs::write(mount.join("w"), b"x").unwrap_err());
    let existing = fs::read_dir(mount)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_file())
        .expect("a file");
    noise.push(
        fs::OpenOptions::new()
            .write(true)
            .open(&existing)
            .unwrap_err(),
    );
    noise.push(fs::remove_dir(mount).unwrap_err());
    for _ in 0..64 {
        let _ = fs::read(&existing);
        let _ = fs::read_dir(mount).map(|entries| entries.count());
    }
    assert!(
        noise.iter().all(|err| err.raw_os_error().is_some()),
        "an invalid operation did not return an errno"
    );

    assert!(
        server.alive(),
        "a shared view panicked and took the server down"
    );
    let stderr = server.stderr_text();
    assert!(
        !stderr.contains("panicked") && !stderr.contains("RUST_BACKTRACE"),
        "panic text in server stderr: {stderr}"
    );
    let after = file_bytes(mount);
    assert!(!after.is_empty(), "the view stopped serving reads");
    server.stop();
}

#[test]
fn mount_many_spec_rejects_unknown_fields_and_duplicates() {
    let fx = build_store();
    let dir = fx.dir.path().join("spec-0");
    fs::create_dir(&dir).unwrap();

    let unknown = fx.dir.path().join("unknown.json");
    fs::write(
        &unknown,
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "views": [{"mountpoint": dir.to_string_lossy(), "writable": true}]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(weft_bin())
        .arg("--store")
        .arg(&fx.store)
        .arg("mount-many")
        .arg("--spec")
        .arg(&unknown)
        .output()
        .expect("run mount-many");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidArgument"));

    let duplicate = fx.dir.path().join("duplicate.json");
    fs::write(
        &duplicate,
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "views": [
                {"mountpoint": dir.to_string_lossy()},
                {"mountpoint": dir.to_string_lossy()}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(weft_bin())
        .arg("--store")
        .arg(&fx.store)
        .arg("mount-many")
        .arg("--spec")
        .arg(&duplicate)
        .output()
        .expect("run mount-many");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("appears twice"));

    // The single-mount CLI is unchanged: an ordinary mount still works.
    let mount_dir = fx.dir.path().join("single-still-works");
    fs::create_dir(&mount_dir).unwrap();
    let mount = Mount::start(&fx.store, &mount_dir, &["--read-only"], &[]);
    assert!(fs::read_dir(&mount_dir).unwrap().count() >= 1);
    drop(mount);
}

#[test]
fn shared_spec_tampering_does_not_start_a_writable_view() {
    // `mount-many` has no writable path at all: even a spec that tries to
    // name a writable view is rejected by the strict schema (no such field),
    // so there is no configuration that turns a shared view writable.
    let fx = build_store();
    let dir = fx.dir.path().join("tamper-0");
    fs::create_dir(&dir).unwrap();
    let spec = fx.dir.path().join("tamper.json");
    fs::write(
        &spec,
        format!(
            "{{\"version\":1,\"views\":[{{\"mountpoint\":\"{}\",\"read_only\":false}}]}}",
            dir.display()
        ),
    )
    .unwrap();
    let output = Command::new(weft_bin())
        .arg("--store")
        .arg(&fx.store)
        .arg("mount-many")
        .arg("--spec")
        .arg(&spec)
        .output()
        .expect("run mount-many");
    assert!(!output.status.success());
    assert!(!is_mounted(&dir));
    let _ = fs::remove_dir(&dir);
}
