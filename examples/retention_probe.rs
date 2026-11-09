//! Cross-language retention probe (subphase R6).
//!
//! Writes and reads the normative `weft.retention.v1` layout so the Go
//! implementation can be compared against Rust byte-for-byte. This is a test
//! and court probe, not a product surface.
//!
//! Usage:
//!   retention_probe write-file  PATH
//!   retention_probe read-file   PATH
//!   retention_probe write-store DIR
//!   retention_probe read-store  DIR
//!   retention_probe rebind-store DIR

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use sha2::{Digest, Sha256};

use weftfs::identity::Hash32;
use weftfs::kv_capability::ATTR_KV_PROV_ROOT;
use weftfs::retention::{
    RetentionBinding, RetentionClass, RetentionPointer, RETENTION_BINDINGS_DIR,
    RETENTION_CURRENT_DIR, RETENTION_DIR,
};
use weftfs::state::ObjectKind;
use weftfs::store::{OpenMode, Options, Store};
use weftfs::Error;

const DESCRIPTOR: [u8; 32] = [0x42; 32];
const ISSUED_AT_NS: i64 = 1_800_000_000_000_000_000;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("retention_probe: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, path] if mode == "write-file" => write_file(Path::new(path)),
        [mode, path] if mode == "read-file" => read_file(Path::new(path)),
        [mode, path] if mode == "write-store" => write_store(Path::new(path)),
        [mode, path] if mode == "read-store" => read_store(Path::new(path)),
        [mode, path] if mode == "rebind-store" => rebind_store(Path::new(path)),
        _ => Err(Error::invalid_argument(
            "usage: retention_probe write-file|read-file|write-store|read-store|rebind-store PATH",
        )),
    }
}

fn descriptor() -> Hash32 {
    Hash32::from_bytes(DESCRIPTOR)
}

fn binding() -> Result<RetentionBinding, Error> {
    RetentionBinding::new(
        descriptor(),
        RetentionClass::Volatile,
        180_000,
        "cross-rust",
        1,
        ISSUED_AT_NS,
        None,
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn report(binding: &RetentionBinding) {
    let bytes = binding.encode().expect("canonical record bytes");
    println!("descriptor_root={}", binding.descriptor_root);
    println!("binding={}", binding.id().expect("binding id"));
    println!("binding_sha256={}", sha256_hex(&bytes));
    println!("binding_bytes={}", bytes.len());
    println!("class={}", binding.class.as_str());
    println!("producer={}", binding.producer);
    println!("policy_epoch={}", binding.policy_epoch);
    println!("issued_at_ns={}", binding.issued_at_ns);
    println!("canonical_hex={}", hex(&bytes));
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn write_file(path: &Path) -> Result<(), Error> {
    let binding = binding()?;
    std::fs::write(path, binding.encode()?)
        .map_err(|err| Error::io(format!("cannot write {}: {err}", path.display())))?;
    report(&binding);
    Ok(())
}

fn read_file(path: &Path) -> Result<(), Error> {
    let bytes = std::fs::read(path)
        .map_err(|err| Error::io(format!("cannot read {}: {err}", path.display())))?;
    let binding = RetentionBinding::decode(&bytes)?;
    let reencoded = binding.encode()?;
    if reencoded != bytes {
        return Err(Error::non_canonical(format!(
            "{} is not canonical retention bytes",
            path.display()
        )));
    }
    report(&binding);
    Ok(())
}

fn ensure_dir(store: &mut Store, parent: Hash32, name: &str) -> Result<Hash32, Error> {
    match store.lookup(parent, name.as_bytes()) {
        Ok(id) => {
            if store.object_meta(id)?.kind != ObjectKind::Dir {
                return Err(Error::not_dir(format!("`{name}` is not a directory")));
            }
            Ok(id)
        }
        Err(err) if err.class() == weftfs::Class::NotFound => {
            store.mkdir(parent, name.as_bytes(), 0o755, 0, 0)
        }
        Err(err) => Err(err),
    }
}

fn write_store(path: &Path) -> Result<(), Error> {
    let mut store = Store::init(
        path,
        Options {
            gc_grace_secs: 0,
            ..Options::default()
        },
    )?;
    let root = store.resolve_path("/")?;
    let mut artifact_attrs = BTreeMap::new();
    artifact_attrs.insert(
        ATTR_KV_PROV_ROOT.to_owned(),
        weftcodec::wcb::Value::Str(descriptor().to_hex()),
    );
    let artifact_object = store.create_file(
        root,
        b"artifact",
        b"retention-probe-artifact",
        0o644,
        0,
        0,
        artifact_attrs,
    )?;
    let artifact = store.object_meta(artifact_object)?.content;

    let retention = ensure_dir(&mut store, root, RETENTION_DIR)?;
    let bindings = ensure_dir(&mut store, retention, RETENTION_BINDINGS_DIR)?;
    let current = ensure_dir(&mut store, retention, RETENTION_CURRENT_DIR)?;

    let binding = binding()?;
    let id = binding.id()?;
    let bytes = binding.encode()?;
    store.create_file(
        bindings,
        id.to_hex().as_bytes(),
        &bytes,
        0o444,
        0,
        0,
        BTreeMap::new(),
    )?;
    let pointer = RetentionPointer {
        descriptor_root: descriptor(),
        binding: id,
        artifact,
    };
    store.create_file(
        current,
        descriptor().to_hex().as_bytes(),
        &pointer.encode(),
        0o644,
        0,
        0,
        BTreeMap::new(),
    )?;
    drop(store);

    println!("artifact={artifact}");
    report(&binding);
    Ok(())
}

fn read_store(path: &Path) -> Result<(), Error> {
    let store = Store::open(path, OpenMode::ReadWrite)?;
    let binding = store
        .retention_of(descriptor())?
        .ok_or_else(|| Error::not_found("no retention binding in the probed store"))?;
    report(&binding);
    Ok(())
}

fn rebind_store(path: &Path) -> Result<(), Error> {
    let mut store = Store::open(path, OpenMode::ReadWrite)?;
    let mut request = weftfs::retention::RetentionRequest::new(
        descriptor(),
        RetentionClass::Ephemeral,
        21_600_000,
        "cross-go",
    );
    request.policy_epoch = 2;
    request.issued_at_ns = Some(ISSUED_AT_NS);
    let id = store.set_retention(request)?;
    let binding = store.retention_binding(id)?;
    report(&binding);
    Ok(())
}
