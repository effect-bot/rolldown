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

    // The Effect service analysis runs in this process; cap its memory so a
    // runaway fails the run instead of exhausting the host. See `memory_guard`.
    // It also has a time limit (`effect_fixture`).
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
    let _memory_guard = effect_fixture.clone().map(memory_guard::guard);

    let started = std::time::Instant::now();
    IntegrationTest::new(meta, self.fixture_path.clone()).run_multiple(configs, plugins).await;
    if let Some(name) = effect_fixture {
      if let Some(failure) =
        effect_fixture::over_time_limit(&name, started.elapsed(), effect_fixture::time_limit())
      {
        panic!("{failure}");
      }
    }
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
