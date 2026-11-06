//! Bounded memory for tests that run an analysis inside the test process.
//!
//! The fixtures and the acceptance set for `experimental.effectServiceShaking`
//! bundle in-process. If the analysis does not terminate and keeps allocating,
//! nothing stops it. On a shared host it uses every free gigabyte before the
//! kernel's OOM killer picks a victim, and the victim may not be the test
//! (EFF-3430: one test reached 45 GiB in 75 seconds).
//!
//! [`guard`] marks a test as guarded. The first guard starts a watchdog thread
//! that samples the process's resident set size. Once it is over the cap, the
//! watchdog prints the guarded tests that are still running and exits the
//! process, so the run fails quickly and says why.
//!
//! The cap is `ROLLDOWN_TEST_MAX_RSS_MB` in MiB, [`DEFAULT_MAX_RSS_MB`] when
//! unset. `0` disables the guard. The cap applies to the whole process, so it
//! is sized for runs filtered to the guarded tests, which is how they are run
//! locally and in the fork's workflow. The guard only works on Linux, where it
//! reads `/proc/self/status`. On other platforms it does nothing.

use std::{
  io::Write as _,
  sync::{Mutex, OnceLock, PoisonError},
  time::Duration,
};

/// Default cap on the test process's resident memory, in MiB. A filtered run
/// of every guarded test peaks well below this; see the measurements in
/// `crates/rolldown/tests/rolldown/tree_shaking/effect_service/README.md`.
pub const DEFAULT_MAX_RSS_MB: u64 = 6144;

const ENV: &str = "ROLLDOWN_TEST_MAX_RSS_MB";
const INTERVAL: Duration = Duration::from_millis(50);

static RUNNING: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
static WATCHDOG: OnceLock<()> = OnceLock::new();

/// A guarded test, from [`guard`] until it is dropped.
#[must_use = "the test is guarded only while the guard is alive"]
pub struct MemoryGuard {
  id: u64,
}

/// Marks `name` as a guarded test until the returned value is dropped, and
/// starts the watchdog if it is not running yet.
pub fn guard(name: impl Into<String>) -> MemoryGuard {
  static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  RUNNING.lock().unwrap_or_else(PoisonError::into_inner).push((id, name.into()));
  WATCHDOG.get_or_init(|| {
    if let Some(limit_mib) = limit_mib() {
      std::thread::Builder::new()
        .name("rolldown-memory-guard".to_string())
        .spawn(move || watch(limit_mib))
        .expect("failed to start the memory guard");
    }
  });
  MemoryGuard { id }
}

impl Drop for MemoryGuard {
  fn drop(&mut self) {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner).retain(|(id, _)| *id != self.id);
  }
}

fn limit_mib() -> Option<u64> {
  let limit = match std::env::var(ENV) {
    Ok(value) => value
      .trim()
      .parse()
      .unwrap_or_else(|_| panic!("{ENV} should be a whole number of MiB, got {value:?}")),
    Err(_) => DEFAULT_MAX_RSS_MB,
  };
  (limit > 0 && rss_kib().is_some()).then_some(limit)
}

#[expect(clippy::exit, reason = "stopping the process is the point of the guard")]
fn watch(limit_mib: u64) {
  loop {
    std::thread::sleep(INTERVAL);
    let Some(rss_kib) = rss_kib() else { return };
    if rss_kib / 1024 <= limit_mib {
      continue;
    }
    let running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner).iter().fold(
      String::new(),
      |mut list, (_, name)| {
        list.push_str("\n  - ");
        list.push_str(name);
        list
      },
    );
    // Written straight to stderr: the test harness captures `eprintln!` output
    // of test threads and threads they spawn, and drops it when the process exits.
    let _ = writeln!(
      std::io::stderr().lock(),
      "\nrolldown memory guard: the test process uses {} MiB, over the {limit_mib} MiB cap \
       ({ENV}). Stopping it before it exhausts the host. Guarded tests still running:{}",
      rss_kib / 1024,
      if running.is_empty() { " none".to_string() } else { running },
    );
    std::process::exit(1);
  }
}

/// Resident set size of this process in KiB, from `/proc/self/status`.
fn rss_kib() -> Option<u64> {
  let status = std::fs::read_to_string("/proc/self/status").ok()?;
  let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
  line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use std::process::Command;

  const CHILD: &str = "ROLLDOWN_MEMORY_GUARD_TEST_CHILD";

  /// A guarded test that keeps allocating is stopped near the cap. The child
  /// gives up at 1 GiB, so a broken guard fails this test instead of running away.
  #[test]
  fn stops_a_runaway_allocation() {
    if std::env::var_os(CHILD).is_some() {
      let _guard = super::guard("runaway");
      let mut blocks = Vec::new();
      for _ in 0..1024 {
        blocks.push(vec![1u8; 1024 * 1024]);
        std::thread::sleep(std::time::Duration::from_millis(1));
      }
      std::hint::black_box(&blocks);
      return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
      .args(["--exact", "memory_guard::tests::stops_a_runaway_allocation", "--nocapture"])
      .env(CHILD, "1")
      .env(super::ENV, "128")
      .output()
      .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "the child allocated 1 GiB without being stopped");
    assert!(stderr.contains("over the 128 MiB cap"), "unexpected stderr:\n{stderr}");
    assert!(stderr.contains("- runaway"), "the guarded test is not named:\n{stderr}");
  }
}
