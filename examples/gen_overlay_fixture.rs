//! Generate the committed `weft.manifest.v4` handoff fixture.
//!
//! There is no v4 write path yet, so this example builds the overlay manifests
//! by hand (chunk blobs plus canonical manifests) and republishes them as file
//! versions through the store's test hook. The resulting directory is a real
//! portable store: the Rust and Go readers both open it and verify the same
//! bytes.
//!
//! Usage: `gen_overlay_fixture <empty-output-dir>`

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use weftfs::format::{
    ChunkRef, ContentIdentity, Manifest, OverlayExtent, OverlayRepresentation, Representation,
    RepresentationKind,
};
use weftfs::identity::{self, Hash32};
use weftfs::recovery::{manifest_path, object_path};
use weftfs::store::{Options, Store};

const CHUNK: u64 = 8;

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| ((index as u32 * 31 + u32::from(seed)) % 251) as u8)
        .collect()
}

fn put_chunk(root: &Path, bytes: &[u8]) -> ChunkRef {
    let hash = identity::h(identity::CHUNK_DOMAIN, bytes);
    let path = object_path(root, hash);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    ChunkRef {
        hash,
        len: bytes.len() as u64,
    }
}

fn put_manifest(root: &Path, manifest: &Manifest) -> Hash32 {
    let bytes = manifest.encode().unwrap();
    let id = identity::manifest_id(&bytes);
    let path = manifest_path(root, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();
    id
}

fn merkle(chunks: &[ChunkRef]) -> Hash32 {
    identity::merkle_root_from_chunks(chunks.iter().map(|chunk| (chunk.len, chunk.hash)))
}

fn make_overlay(
    base_id: Hash32,
    base: &Manifest,
    base_table: &[ChunkRef],
    extent: OverlayExtent,
    size: u64,
    depth: u64,
    base_overlay_bytes: u64,
) -> (Manifest, Vec<ChunkRef>) {
    let mut table = base_table.to_vec();
    let start = usize::try_from(extent.offset / CHUNK).unwrap();
    let end = start + extent.chunks.len();
    if end > table.len() {
        table.splice(start..table.len(), extent.chunks.iter().cloned());
    } else {
        table.splice(start..end, extent.chunks.iter().cloned());
    }
    let manifest = Manifest {
        size,
        chunk_size: CHUNK,
        chunks: Vec::new(),
        content_hash: merkle(&table),
        representation: Some(Representation {
            kind: RepresentationKind::Overlay,
            frame: None,
            dependencies: Vec::new(),
            strategy: "overlay".to_owned(),
            decoded_size: size,
            tensor: None,
            overlay: Some(OverlayRepresentation {
                base: base_id,
                base_content_root: base.content_hash,
                base_size: base.size,
                depth,
                overlay_bytes: base_overlay_bytes + extent.length,
                extents: vec![extent],
            }),
        }),
        identity: ContentIdentity::MerkleV3,
    };
    (manifest, table)
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: gen_overlay_fixture <dir>");
    let out = Path::new(&out);
    if out.exists()
        && fs::read_dir(out)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        panic!("output directory {} is not empty", out.display());
    }
    fs::create_dir_all(out).unwrap();

    let options = Options {
        owner_uid: 0,
        owner_gid: 0,
        chunk_size: CHUNK,
        checkpoint_interval: 256,
        gc_grace_secs: 0,
        fixed_timestamp_ns: Some(1_700_000_000_000_000_000),
        content_identity: ContentIdentity::MerkleV3,
        // The committed overlay fixture is frozen v1 commit bytes.
        attest_indexes: false,
        ..Options::default()
    };
    let mut store = Store::init(out, options).unwrap();
    let root = store.resolve_path("/").unwrap();

    let base_bytes = pattern(32, 1);
    let base_object = store
        .create_file(root, b"base", &base_bytes, 0o644, 0, 0, BTreeMap::new())
        .unwrap();
    let base_record = store.object_meta(base_object).unwrap();
    let base_manifest = store.manifest_of(base_record.content).unwrap();

    let overlay_chunk = put_chunk(out, &pattern(8, 2));
    let (overlay_manifest, overlay_table) = make_overlay(
        base_record.content,
        &base_manifest,
        &base_manifest.chunks,
        OverlayExtent {
            offset: CHUNK,
            length: CHUNK,
            chunks: vec![overlay_chunk.clone()],
        },
        32,
        1,
        0,
    );
    let overlay_id = put_manifest(out, &overlay_manifest);
    let overlay_object = store
        .create_file(
            root,
            b"overlay",
            b"seed-overlay",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .publish_manifest_for_test(overlay_object, overlay_id, 32)
        .unwrap();

    let nested_chunk = put_chunk(out, &pattern(8, 3));
    let (nested_manifest, nested_table) = make_overlay(
        overlay_id,
        &overlay_manifest,
        &overlay_table,
        OverlayExtent {
            offset: 24,
            length: CHUNK,
            chunks: vec![nested_chunk.clone()],
        },
        32,
        2,
        overlay_manifest
            .representation
            .as_ref()
            .unwrap()
            .overlay
            .as_ref()
            .unwrap()
            .overlay_bytes,
    );
    let nested_id = put_manifest(out, &nested_manifest);
    let nested_object = store
        .create_file(
            root,
            b"nested",
            b"seed-nested",
            0o644,
            0,
            0,
            BTreeMap::new(),
        )
        .unwrap();
    store
        .publish_manifest_for_test(nested_object, nested_id, 32)
        .unwrap();

    let mut overlay_bytes = base_bytes.clone();
    overlay_bytes[8..16].copy_from_slice(&pattern(8, 2));
    let mut nested_bytes = overlay_bytes.clone();
    nested_bytes[24..32].copy_from_slice(&pattern(8, 3));
    assert_eq!(store.read_object(base_object).unwrap(), base_bytes);
    assert_eq!(store.read_object(overlay_object).unwrap(), overlay_bytes);
    assert_eq!(store.read_object(nested_object).unwrap(), nested_bytes);

    let objects = serde_json::json!([
        {
            "path": "/base",
            "size": base_bytes.len(),
            "content_root": base_record.content.to_hex(),
            "expected_hex": hex::encode(&base_bytes),
        },
        {
            "path": "/overlay",
            "size": overlay_bytes.len(),
            "content_root": overlay_id.to_hex(),
            "expected_hex": hex::encode(&overlay_bytes),
        },
        {
            "path": "/nested",
            "size": nested_bytes.len(),
            "content_root": nested_id.to_hex(),
            "expected_hex": hex::encode(&nested_bytes),
        },
    ]);
    let description = serde_json::json!({
        "format": "weftfs-overlay-fixture-v1",
        "chunk_size": CHUNK,
        "base_manifest": base_record.content.to_hex(),
        "overlay_manifest": overlay_id.to_hex(),
        "nested_manifest": nested_id.to_hex(),
        "objects": objects,
        "base_chunks": base_manifest.chunks.iter().map(|c| c.hash.to_hex()).collect::<Vec<_>>(),
        "overlay_chunks": overlay_table.iter().map(|c| c.hash.to_hex()).collect::<Vec<_>>(),
        "nested_chunks": nested_table.iter().map(|c| c.hash.to_hex()).collect::<Vec<_>>(),
    });
    fs::write(
        out.join("EXPECTED.json"),
        serde_json::to_vec_pretty(&description).unwrap(),
    )
    .unwrap();
    println!("fixture={} overlay={}", out.display(), overlay_id.to_hex());
}
