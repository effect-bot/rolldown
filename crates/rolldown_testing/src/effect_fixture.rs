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
//! takes about 0.6 s in a parallel run of the suite on a 16-core host.

use std::time::Duration;

/// Entries budget (points-to and copy entries) for a fixture's analysis.
pub const MAX_ENTRIES: u64 = 100_000;

/// Default time limit for one fixture, in milliseconds.
pub const DEFAULT_TIME_LIMIT_MS: u64 = 2_000;

const TIME_LIMIT_ENV: &str = "ROLLDOWN_TEST_EFFECT_FIXTURE_TIME_LIMIT_MS";

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

/// Why the fixture `name` failed its time limit, if it did.
pub fn over_time_limit(name: &str, elapsed: Duration, limit: Option<Duration>) -> Option<String> {
  let limit = limit?;
  (elapsed > limit).then(|| {
    format!(
      "{name} took {:.2} s, over the {:.2} s limit for an Effect service fixture ({TIME_LIMIT_ENV})",
      elapsed.as_secs_f64(),
      limit.as_secs_f64()
    )
  })
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::over_time_limit;

  #[test]
  fn fails_only_past_the_limit() {
    let limit = Some(Duration::from_secs(2));
    assert_eq!(over_time_limit("f", Duration::from_millis(1_999), limit), None);
    assert_eq!(over_time_limit("f", Duration::from_secs(2), limit), None);
    let message = over_time_limit("f", Duration::from_millis(2_500), limit).unwrap();
    assert!(message.starts_with("f took 2.50 s, over the 2.00 s limit"), "{message}");
    assert_eq!(over_time_limit("f", Duration::from_secs(60), None), None);
  }
}
