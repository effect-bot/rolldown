//! Limits for the fixtures that run `experimental.effectServiceShaking`.
//!
//! Each fixture is a small program, so its analysis gets a small entries
//! budget, [`MAX_ENTRIES`], unless its config sets
//! `effectServiceShakingMaxEntries` itself (the dedicated budget fixture,
//! `tree_shaking/effect_service/budget_exceeded_keeps_every_method`, does).
//! The largest fixture solve takes about 17k entries, so a fixture that needs
//! more than the budget has regressed. The analysis's own default, 32M, is for
//! real programs.
//!
//! Each fixture must also finish within a time limit, measured over the whole
//! fixture: every build and the run of its `_test.mjs`. The limit is
//! `ROLLDOWN_TEST_EFFECT_FIXTURE_TIME_LIMIT_MS` in milliseconds,
//! [`DEFAULT_TIME_LIMIT_MS`] when unset. `0` disables it. The slowest fixture
//! takes about 0.7 s in a parallel run of the suite on a 16-core host.
//!
//! The limit is a deadline, not a measurement taken afterwards. A hung
//! analysis blocks its thread and a hung `_test.mjs` blocks the wait for
//! Node, so nothing inside the test's own process can stop them. Instead the
//! test runs the fixture again in a child process, the same test binary
//! filtered to the same test ([`run_in_child`]). The child is the leader of
//! a new process group, which Node and anything it starts join, and the
//! parent kills the whole group at the deadline.

use std::{
  fs::File,
  process::{Command, ExitStatus, Stdio},
  sync::atomic::{AtomicU64, Ordering},
  time::{Duration, Instant},
};

/// Entries budget (points-to and copy entries) for a fixture's analysis.
pub const MAX_ENTRIES: u64 = 100_000;

/// Default time limit for one fixture, in milliseconds.
pub const DEFAULT_TIME_LIMIT_MS: u64 = 2_000;

const TIME_LIMIT_ENV: &str = "ROLLDOWN_TEST_EFFECT_FIXTURE_TIME_LIMIT_MS";

/// Set in the child process that runs one fixture for its parent.
const CHILD_ENV: &str = "ROLLDOWN_TEST_EFFECT_FIXTURE_CHILD";

/// The time limit from the environment, `None` when disabled.
pub fn time_limit() -> Option<Duration> {
  let ms = match std::env::var(TIME_LIMIT_ENV) {
    Ok(value) => value.trim().parse().unwrap_or_else(|_| {
      panic!("{TIME_LIMIT_ENV} should be a whole number of milliseconds, got {value:?}")
    }),
    Err(_) => DEFAULT_TIME_LIMIT_MS,
  };
  (ms > 0).then(|| Duration::from_millis(ms))
}

/// Whether this process is the child that runs one fixture ([`run_in_child`]).
pub fn is_child() -> bool {
  std::env::var_os(CHILD_ENV).is_some()
}

/// Runs the fixture `name` by running the libtest test `test` of this binary in a
/// child process, and panics with the child's output if the test fails or is still running at
/// `limit`. At the deadline the child and every process it started are killed first.
pub fn run_in_child(name: &str, test: &str, limit: Duration) {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  let log_path = std::env::temp_dir().join(format!(
    "rolldown-effect-fixture-{}-{}.log",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ));
  let log = File::create(&log_path).expect("failed to create the fixture's log file");
  let mut command = Command::new(std::env::current_exe().expect("no path to the test binary"));
  command
    .args([test, "--exact", "--test-threads=1"])
    .env(CHILD_ENV, "1")
    .stdin(Stdio::null())
    .stdout(log.try_clone().expect("failed to share the fixture's log file"))
    .stderr(log);
  let started = Instant::now();
  let status = run_with_deadline(command, limit);
  let output = std::fs::read_to_string(&log_path).unwrap_or_default();
  let _ = std::fs::remove_file(&log_path);
  match status {
    Some(status) if status.success() && output.contains(" 1 passed;") => {}
    Some(status) => panic!("{name} failed in its child process ({status}):\n{output}"),
    None => panic!(
      "{name} did not finish within the {:.2} s time limit for an Effect service fixture \
       ({TIME_LIMIT_ENV}); stopped it and its child processes after {:.2} s. Its output:\n{output}",
      limit.as_secs_f64(),
      started.elapsed().as_secs_f64(),
    ),
  }
}

/// Runs `command` as the leader of a new process group and waits for it until `limit`
/// has passed. Returns its exit status, or `None` when the deadline came first; then the
/// whole group has been killed.
pub fn run_with_deadline(mut command: Command, limit: Duration) -> Option<ExitStatus> {
  #[cfg(unix)]
  std::os::unix::process::CommandExt::process_group(&mut command, 0);
  let mut child = command.spawn().expect("failed to start the fixture's process");
  let deadline = Instant::now() + limit;
  loop {
    if let Some(status) = child.try_wait().expect("failed to wait for the fixture's process") {
      return Some(status);
    }
    if Instant::now() >= deadline {
      // The group's id is the leader's pid; a negative pid signals the whole group.
      #[cfg(unix)]
      let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stderr(Stdio::null())
        .status();
      let _ = child.kill();
      let _ = child.wait();
      return None;
    }
    std::thread::sleep(Duration::from_millis(5));
  }
}
