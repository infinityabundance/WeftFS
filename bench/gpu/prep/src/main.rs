//! Dump a mode-9 (FLOAT_TENSOR) frame plus its exact plaintext, for the T046 GPU court.
//!
//! This tool is a measurement harness, not product code. It uses the shipped
//! encoder/decoder so the frame under test is the real one.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use weftcodec::modes::{decode_float_tensor, encode_float_tensor, FloatDtype};
use weftcodec::{Frame, Limits};

/// Deterministic BF16 weights: varied exponents, no dependence on the clock.
fn tensor(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 2);
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in 0..n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Bias the value into a plausible weight range: exponent around 0x3F,
        // with the low bits free so the plane table is non-trivial.
        let jitter = ((state >> 33) as u16) & 0x07FF;
        let word: u16 = 0x3F00u16.wrapping_add(jitter) ^ ((i as u16) & 0x000F);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("out dir"));
    let n: usize = args
        .next()
        .unwrap_or_else(|| "16777216".into())
        .parse()
        .expect("elements");

    fs::create_dir_all(&out).expect("out dir");

    let plain = tensor(n);

    let t0 = Instant::now();
    let frame_bytes =
        encode_float_tensor(&plain, FloatDtype::Bf16, &[n as u64], &Limits::default())
            .expect("encode");
    let encode_ms = t0.elapsed().as_secs_f64() * 1e3;

    let frame = Frame::parse_default(&frame_bytes).expect("parse");
    let body = frame.body;
    let desc_len = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let mant_len = n; // BF16 sign+mantissa plane is one byte per element
    let exp_len = body
        .len()
        .checked_sub(4 + desc_len + mant_len)
        .expect("body layout");

    let t1 = Instant::now();
    let decoded = decode_float_tensor(&frame, &Limits::default()).expect("decode");
    let decode_ms = t1.elapsed().as_secs_f64() * 1e3;
    assert_eq!(decoded, plain, "decoder must reproduce the source exactly");

    fs::write(out.join("plain.bin"), &plain).unwrap();
    fs::write(out.join("frame.bin"), &frame_bytes).unwrap();
    fs::write(out.join("body.bin"), body).unwrap();
    fs::write(
        out.join("meta.txt"),
        format!(
            "elements={n}\ndesc_len={desc_len}\nexp_len={exp_len}\nmant_len={mant_len}\nframe_len={}\nencode_ms={encode_ms:.3}\ncpu_decode_ms={decode_ms:.3}\n",
            frame_bytes.len()
        ),
    )
    .unwrap();

    println!(
        "elements={n} frame_len={} body_len={} desc_len={desc_len} exp_len={exp_len} mant_len={mant_len} encode_ms={encode_ms:.3} cpu_decode_ms={decode_ms:.3}",
        frame_bytes.len(),
        body.len()
    );
}
