use std::{
  path::{Path, PathBuf},
  sync::OnceLock,
};

use crate::{
  effect_fixture,
  integration_test::{IntegrationTest, NamedBundlerOptions},
  memory_guard,
  test_config::read_test_config,
};
use rolldown::{BundlerOptions, plugin::__inner::SharedPluginable};
use rolldown_common::{ExperimentalOptions, PreserveEntrySignatures};
use rolldown_testing_config::{ConfigVariant, TestConfig, TestMeta};

pub struct Fixture {
  config_path: PathBuf,
  fixture_path: PathBuf,
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn shared_runtime() -> &'static tokio::runtime::Runtime {
  RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

// Using std once lock to store env variable
static NEEDS_EXTENDED_TESTS: OnceLock<bool> = OnceLock::new();
/// A function to get the API key.
///
/// This is the idiomatic way to wrap the OnceLock, providing a clean,
/// easy-to-use interface.
fn needs_extended_tests() -> bool {
  *NEEDS_EXTENDED_TESTS
    .get_or_init(|| std::env::var("NEEDS_EXTENDED").ok().unwrap_or("true".to_owned()) == "true")
}

impl Fixture {
  pub fn new(path: impl AsRef<Path>) -> Self {
    // Paths could be UNC format in windows, see https://github.com/rust-lang/rust/issues/42869 for more details
    let path = dunce::simplified(path.as_ref());

    Self { fixture_path: path.to_path_buf(), config_path: path.join("_config.json") }
  }

  pub fn run_integration_test(self) {
    shared_runtime().block_on(self.run_inner(vec![]));
  }

  pub fn run_integration_test_with_plugins(self, plugins: Vec<SharedPluginable>) {
    shared_runtime().block_on(self.run_inner(plugins));
  }

  async fn run_inner(self, plugins: Vec<SharedPluginable>) {
    let TestConfig { config: mut options, meta, mut config_variants } =
      read_test_config(&self.config_path);

    if options.cwd.is_none() {
      options.cwd = Some(self.fixture_path.clone());
    }

    // Small programs get a small budget; see `effect_fixture`.
    if let Some(experimental) = options.experimental.as_mut() {
      experimental.effect_service_shaking_max_entries.get_or_insert(effect_fixture::MAX_ENTRIES);
    }

    if needs_extended_tests() {
      Self::apply_extended_tests(&meta, &options, &mut config_variants);
    }

    let configs = std::iter::once(NamedBundlerOptions {
      options: options.clone(),
      description: None,
      snapshot: None,
      config_name: None,
      expect_execution_failure: meta.expect_execution_failure.clone(),
    })
    .chain(config_variants.into_iter().map(|variant| NamedBundlerOptions {
      options: variant.apply(&options),
      description: Some(variant.description()),
      snapshot: variant.snapshot,
      config_name: variant.config_name,
      expect_execution_failure: variant.expect_execution_failure,
    }))
    .collect::<Vec<_>>();

    // Fixtures that run the Effect service analysis get a time limit and a memory cap.
    let effect_fixture = configs
      .iter()
      .any(|config| {
        config
          .options
          .experimental
          .as_ref()
          .is_some_and(ExperimentalOptions::is_effect_service_shaking_enabled)
      })
      .then(|| {
        let workspace = Path::new(env!("WORKSPACE_DIR"));
        let name = self.fixture_path.strip_prefix(workspace).unwrap_or(&self.fixture_path);
        name.to_string_lossy().into_owned()
      });
    // The time limit is enforced from outside: the fixture runs in a child process that is
    // killed at the deadline (`effect_fixture::run_in_child`). libtest names each test's thread
    // after the test, which is how the child finds this test again.
    if let Some(name) = &effect_fixture {
      let test = std::thread::current().name().map(ToString::to_string);
      if let (false, Some(limit), Some(test)) =
        (effect_fixture::is_child(), effect_fixture::time_limit(), test)
      {
        effect_fixture::run_in_child(name, &test, limit);
        return;
      }
    }
    // The analysis runs in this process; cap its memory so a runaway fails the run instead of
    // exhausting the host. See `memory_guard`.
    let _memory_guard = effect_fixture.map(memory_guard::guard);

    IntegrationTest::new(meta, self.fixture_path.clone()).run_multiple(configs, plugins).await;
  }

  fn apply_extended_tests(
    meta: &TestMeta,
    options: &BundlerOptions,
    config_variants: &mut Vec<ConfigVariant>,
  ) {
    use rolldown::determine_minify_internal_exports_default;
    use rolldown_common::RawMinifyOptions;

    if meta.extended_tests.opposite_minify_internal_exports
      && options.minify_internal_exports.is_none()
    {
      // Determine what the default value would be based on format and minify settings
      let default_value = determine_minify_internal_exports_default(
        options.format,
        options.minify.as_ref().unwrap_or(&RawMinifyOptions::Bool(false)),
      );

      // Test the opposite of the default
      let test_value = !default_value;

      config_variants.push(ConfigVariant {
        config_name: Some("extended-minify-internal-exports".to_string()),
        minify_internal_exports: Some(test_value),
        snapshot: Some(false),
        expect_execution_failure: meta.expect_execution_failure.clone(),
        ..Default::default()
      });
    }

    if meta.extended_tests.preserve_entry_signatures_strict
      && options.preserve_entry_signatures.is_none()
    {
      config_variants.push(ConfigVariant {
        config_name: Some("extended-preserve-entry-signatures-strict".to_string()),
        preserve_entry_signatures: Some(PreserveEntrySignatures::Strict),
        snapshot: Some(false),
        expect_execution_failure: meta.expect_execution_failure.clone(),
        ..Default::default()
      });
    }

    if meta.extended_tests.preserve_entry_signatures_allow_extension
      && options.preserve_entry_signatures.is_none()
    {
      config_variants.push(ConfigVariant {
        config_name: Some("extended-preserve-entry-signatures-allow-extension".to_string()),
        preserve_entry_signatures: Some(PreserveEntrySignatures::AllowExtension),
        snapshot: Some(false),
        expect_execution_failure: meta.expect_execution_failure.clone(),
        ..Default::default()
      });
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
  };

  use rolldown::plugin::{
    __inner::{Pluginable, SharedPluginable},
    HookBuildStartArgs, HookNoopReturn, HookUsage, Plugin, PluginContext,
  };

  use super::Fixture;
  use crate::effect_fixture;

  fn hung_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/effect_fixture_hangs")
  }

  /// Runs the fixture on a thread named after this test, so the harness can tell which test it
  /// is, and returns its panic message, or fails when it is still running after the limit and a
  /// few seconds of slack.
  fn run_hung(plugins: Vec<SharedPluginable>) -> String {
    let limit = effect_fixture::time_limit().expect("the time limit is on");
    let name = std::thread::current().name().expect("libtest names test threads").to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
      .name(name)
      .spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          Fixture::new(hung_fixture()).run_integration_test_with_plugins(plugins);
        }));
        let message = result.err().map(|err| {
          err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(ToString::to_string))
            .unwrap_or_default()
        });
        let _ = tx.send(message);
      })
      .unwrap();
    let bound = limit + Duration::from_secs(3);
    match rx.recv_timeout(bound) {
      Ok(Some(message)) => message,
      Ok(None) => panic!("the hung fixture passed"),
      Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
        panic!("the hung fixture was still running {} s after it started", bound.as_secs())
      }
    }
  }

  fn running(pid: u32) -> bool {
    // A killed process stays a zombie until whoever adopted it reaps it.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
      stat.rsplit(')').next().is_some_and(|rest| !rest.trim_start().starts_with('Z'))
    })
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn a_hung_test_script_and_its_children_are_stopped() {
    let pid_file = hung_fixture().join("dist/sleeper.pid");
    let _ = std::fs::remove_file(&pid_file);
    let started = Instant::now();
    let message = run_hung(vec![]);
    assert!(message.contains("time limit"), "{message}");
    let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    let _ = std::fs::remove_file(&pid_file);
    while running(pid) && started.elapsed() < Duration::from_secs(10) {
      std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!running(pid), "the test script's child process {pid} is still running");
  }

  /// Blocks its thread in the build, as a runaway analysis would.
  #[derive(Debug)]
  struct Spin;

  impl Plugin for Spin {
    fn name(&self) -> Cow<'static, str> {
      "spin".into()
    }

    fn register_hook_usage(&self) -> HookUsage {
      HookUsage::BuildStart
    }

    async fn build_start(
      &self,
      _ctx: &PluginContext,
      _args: &HookBuildStartArgs<'_>,
    ) -> HookNoopReturn {
      loop {
        std::hint::spin_loop();
      }
    }
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn a_build_that_never_yields_is_stopped() {
    let message = run_hung(vec![Pluginable::new_shared(Spin)]);
    assert!(message.contains("time limit"), "{message}");
  }
}
