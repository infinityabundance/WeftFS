//! Deterministic byte-fixture generator for the showcase negative workloads.
//!
//! The output is a pure function of `seed` and `length` (xorshift64*), so a
//! re-run produces byte-identical fixtures without importing a PRNG crate.

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: pseudorandom <seed-u64> <length> <output>");
        std::process::exit(64);
    }
    let seed: u64 = args[1].parse().expect("seed must be a u64");
    let length: usize = args[2].parse().expect("length must be a usize");
    let path = &args[3];

    let mut state = seed | 1;
    let mut out = BufWriter::new(File::create(path).expect("create output"));
    let mut chunk = [0u8; 8192];
    let mut remaining = length;
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        for byte in chunk.iter_mut().take(take) {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
        out.write_all(&chunk[..take]).expect("write fixture");
        remaining -= take;
    }
    out.flush().expect("flush fixture");
}
