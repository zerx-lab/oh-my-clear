//! [`JobCtx`]: cancellation flag and progress counters shared between a running job and
//! the engine, which samples [`JobCtx::snapshot`] (~10 Hz) into `job` events. Scanner
//! threads only touch atomics, so reporting costs nothing measurable per file.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use omc_proto::jobs::{Phase, Progress};
use parking_lot::Mutex;

/// Every this many directories, the walker publishes the one it is in.
const CURRENT_EVERY: u64 = 64;

/// Cancellation and progress of one job.
#[derive(Debug, Default)]
pub struct JobCtx {
    cancelled: AtomicBool,
    phase: AtomicU8,
    items: AtomicU64,
    bytes: AtomicU64,
    done: AtomicU64,
    total: AtomicU64,
    dirs: AtomicU64,
    current: Mutex<Option<String>>,
}

impl JobCtx {
    /// A fresh context (phase `starting`, all counters 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the job to stop at the next check.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether [`Self::cancel`] was called. Long loops check this per directory or item.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Enters `phase`.
    pub fn set_phase(&self, phase: Phase) {
        self.phase.store(phase_to_u8(phase), Ordering::Relaxed);
    }

    /// Counts `n` visited entries.
    pub fn add_items(&self, n: u64) {
        self.items.fetch_add(n, Ordering::Relaxed);
    }

    /// Counts `n` bytes found or freed.
    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Sets the byte counter (e.g. when a phase restarts counting).
    pub fn set_bytes(&self, n: u64) {
        self.bytes.store(n, Ordering::Relaxed);
    }

    /// Sets the amount of countable work and resets `done`.
    pub fn set_total(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// Counts `n` finished units of the countable work.
    pub fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Publishes what is being processed (display only).
    pub fn set_current(&self, what: impl Into<String>) {
        *self.current.lock() = Some(what.into());
    }

    /// Called by the walker per directory: publishes every [`CURRENT_EVERY`]th one.
    pub(crate) fn entered_dir(&self, dir: &Path) {
        let n = self.dirs.fetch_add(1, Ordering::Relaxed);
        if n.is_multiple_of(CURRENT_EVERY)
            && let Some(mut current) = self.current.try_lock()
        {
            *current = Some(dir.display().to_string());
        }
    }

    /// The counters now.
    pub fn snapshot(&self) -> Progress {
        Progress {
            phase: phase_from_u8(self.phase.load(Ordering::Relaxed)),
            items: self.items.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            current: self.current.lock().clone(),
        }
    }
}

const PHASES: [Phase; 9] = [
    Phase::Starting,
    Phase::Scanning,
    Phase::Measuring,
    Phase::Hashing,
    Phase::Quitting,
    Phase::Uninstalling,
    Phase::Elevating,
    Phase::Removing,
    Phase::Finishing,
];

fn phase_to_u8(phase: Phase) -> u8 {
    PHASES
        .iter()
        .position(|p| *p == phase)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0)
}

fn phase_from_u8(n: u8) -> Phase {
    PHASES.get(usize::from(n)).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_every_counter() {
        let ctx = JobCtx::new();
        ctx.set_phase(Phase::Hashing);
        ctx.add_items(3);
        ctx.add_bytes(10);
        ctx.set_total(4);
        ctx.add_done(1);
        ctx.set_current("x");
        let p = ctx.snapshot();
        assert_eq!(p.phase, Phase::Hashing, "phase round-trips");
        assert_eq!(
            (p.items, p.bytes, p.done, p.total),
            (3, 10, 1, 4),
            "counters"
        );
        assert_eq!(p.current.as_deref(), Some("x"), "current item");
        assert!(!ctx.is_cancelled(), "not cancelled yet");
        ctx.cancel();
        assert!(ctx.is_cancelled(), "cancel is observable");
    }

    #[test]
    fn every_phase_round_trips() {
        for phase in PHASES {
            assert_eq!(phase_from_u8(phase_to_u8(phase)), phase, "{phase:?}");
        }
    }
}
