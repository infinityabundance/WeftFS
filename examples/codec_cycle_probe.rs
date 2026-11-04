//! Deterministic fixture generator for the codec-cycle rejection showcase step.
//!
//! Two structurally valid WFT1 REFERENCE frames are written whose dependency
//! tables point at each other. Such a graph cannot be produced by ordinary
//! content-addressed encoding (a frame names its base by plaintext hash), so a
//! deliberately forged pair is required to exercise the cycle detector in
//! `weftcodec archive create`. This binary only writes bytes; the real CLI is
//! what must reject them.

use std::env;
use std::fs;

use weftcodec::frame::{
    encode_frame, Dependency, FrameParts, Mode, DEP_DOMAIN_PAYLOAD, DOMAIN_PAYLOAD, FORMAT_MAJOR,
    FORMAT_MINOR,
};
use weftcodec::{Hash32, Limits};

fn main() {
    let mut out_dir = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--out" {
            out_dir = args.next();
        }
    }
    let out_dir = out_dir.expect("usage: codec_cycle_probe --out DIR");
    fs::create_dir_all(&out_dir).expect("create output directory");

    let limits = Limits::default();
    let a_hash = Hash32::from_bytes([1u8; 32]);
    let b_hash = Hash32::from_bytes([2u8; 32]);
    let dep = |hash: Hash32| Dependency::new(hash, 1, DEP_DOMAIN_PAYLOAD, 0);

    let a = encode_frame(
        &FrameParts {
            format_major: FORMAT_MAJOR,
            format_minor: FORMAT_MINOR,
            domain: DOMAIN_PAYLOAD,
            mode: Mode::Reference,
            flags: 0,
            level: 0,
            decoded_len: 1,
            content_hash: a_hash,
            dependencies: &[dep(b_hash)],
            chunks: &[],
            body: &[],
        },
        &limits,
    )
    .expect("encode cycle frame A");

    let b = encode_frame(
        &FrameParts {
            format_major: FORMAT_MAJOR,
            format_minor: FORMAT_MINOR,
            domain: DOMAIN_PAYLOAD,
            mode: Mode::Reference,
            flags: 0,
            level: 0,
            decoded_len: 1,
            content_hash: b_hash,
            dependencies: &[dep(a_hash)],
            chunks: &[],
            body: &[],
        },
        &limits,
    )
    .expect("encode cycle frame B");

    fs::write(format!("{out_dir}/cycle-a.wft"), &a).expect("write cycle-a.wft");
    fs::write(format!("{out_dir}/cycle-b.wft"), &b).expect("write cycle-b.wft");
    println!("cycle_a_hash={} cycle_b_hash={}", a_hash, b_hash);
}
