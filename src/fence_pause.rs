//! Process-fault hook for the fenced-writer court (subphase R27).
//!
//! The court needs to suspend a writer at an exact point: *after* it has proven
//! its lease with a conditional write on the `LOCK` record and *before* it
//! publishes a head. The hook below is inert unless `WEFT_FENCE_PAUSE_AFTER_FENCE`
//! is set, so it never affects a normal build or a normal test run.
//!
//! When enabled, the process:
//!
//! 1. writes a small JSON line to `WEFT_FENCE_AFTER_FENCE_READY` (pid, fence,
//!    generation, head) so the parent knows the fence is durable, and
//! 2. stops itself with `SIGSTOP`.
//!
//! The parent lets the lease expire, takes it over with a second writer, and
//! sends `SIGCONT`. The resumed writer must then fail `Busy` and leave the head
//! unchanged. This is the interleaving the external review found uncovered:
//! the old protocol wrote the fence and the head in two different objects, so
//! a takeover between them could not stop the stale head write.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::identity::Hash32;

/// Whether the court hook is armed.
pub fn armed() -> bool {
    std::env::var_os("WEFT_FENCE_PAUSE_AFTER_FENCE").is_some()
}

static PAUSED_ONCE: AtomicBool = AtomicBool::new(false);

/// Pause at the publication boundary. `generation` and `head` describe the
/// store state *before* this publication (its own generation and head).
#[allow(unsafe_code)]
pub(crate) fn after_fence(generation: u64, head: &Hash32) {
    if !armed() || PAUSED_ONCE.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Ok(path) = std::env::var("WEFT_FENCE_AFTER_FENCE_READY") {
        if !path.is_empty() {
            // Best effort: a failed write makes the parent time out and fail
            // the test, which is the honest outcome.
            if let Ok(mut file) = std::fs::File::create(&path) {
                let _ = writeln!(
                    file,
                    "{{\"pid\":{},\"generation\":{},\"head\":\"{}\"}}",
                    std::process::id(),
                    generation,
                    head.to_hex()
                );
                let _ = file.flush();
            }
        }
    }
    // SIGSTOP is not catchable and does not release the process's resources.
    unsafe { libc::raise(libc::SIGSTOP) };
}
