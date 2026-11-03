//! P5 concurrency: several readers and one writer through FUSE. Each write is
//! one atomic commit and each read is served from one immutable content root,
//! so a reader must never observe a mixed ("torn") version.

#![cfg(feature = "fuse")]

mod fuse_common;

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use fuse_common::Fixture;

const SIZE: usize = 32 * 1024;
const A: u8 = 0xAA;
const B: u8 = 0xBB;

#[test]
fn concurrent_readers_and_one_writer_see_whole_versions() {
    let fx = Fixture::new();
    let mount = fx.mount(&[]);
    let path = fx.path("hot.bin");
    fs::write(&path, vec![A; SIZE]).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Result<(), String>>();

    let mut readers = Vec::new();
    for _ in 0..4 {
        let path = path.clone();
        let stop = Arc::clone(&stop);
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(err) => {
                    let _ = tx.send(Err(format!("open: {err}")));
                    return;
                }
            };
            let mut buffer = vec![0u8; SIZE];
            let mut reads = 0u64;
            while !stop.load(Ordering::SeqCst) {
                match file.read_at(&mut buffer, 0) {
                    Ok(n) => {
                        reads += 1;
                        if n != SIZE {
                            let _ = tx.send(Err(format!("short read {n} of {SIZE}")));
                            return;
                        }
                        let first = buffer[0];
                        if first != A && first != B {
                            let _ = tx.send(Err(format!("unexpected byte {first:#x}")));
                            return;
                        }
                        if let Some(index) = buffer.iter().position(|byte| *byte != first) {
                            let _ = tx.send(Err(format!(
                                "torn read at byte {index}: {first:#x} then {:#x}",
                                buffer[index]
                            )));
                            return;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(format!("read: {err}")));
                        return;
                    }
                }
            }
            let _ = reads;
            let _ = tx.send(Ok(()));
        }));
    }
    drop(tx);

    let writer = OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap_or_else(|err| panic!("writer open failed: {err}; daemon: {}", mount.stderr_text()));
    for iteration in 0..20 {
        let byte = if iteration % 2 == 0 { B } else { A };
        writer
            .write_all_at(&vec![byte; SIZE], 0)
            .unwrap_or_else(|err| {
                panic!(
                    "write iteration {iteration} failed: {err}; daemon: {}",
                    mount.stderr_text()
                )
            });
    }
    stop.store(true, Ordering::SeqCst);

    for reader in readers {
        reader.join().expect("reader thread panicked");
    }
    let mut failures = Vec::new();
    for _ in 0..4 {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => failures.push(message),
            Err(err) => failures.push(format!("reader did not report: {err}")),
        }
    }
    assert!(failures.is_empty(), "reader failures: {failures:?}");

    let final_bytes = fs::read(&path).unwrap();
    assert_eq!(final_bytes.len(), SIZE);
    let first = final_bytes[0];
    assert!(first == A || first == B);
    assert!(final_bytes.iter().all(|byte| *byte == first));
}
