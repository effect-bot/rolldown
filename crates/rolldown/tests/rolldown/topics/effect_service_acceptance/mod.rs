//! Acceptance set for `experimental.effectServiceShaking` (EFF-3430).
//!
//! Each program under `programs/<name>/` is bundled twice, with the pass off
//! and on, and both bundles are run with Node. A program passes when:
//!
//! - both runs exit 0 with identical stdout (behaviour is unchanged);
//! - every name in `expect.json#keep` is still in the enabled bundle;
//! - no name in `expect.json#remove` is in the enabled bundle;
//! - the enabled bundle is at least `minReductionPercent` smaller, when set;
//! - an `EFFECT_SERVICE_SHAKING` warning is emitted, when `expectDiagnostic` is set.
//!
//! The two bundles are written to `dist/off/main.js` and `dist/on/main.js` in
//! the program directory. `run` names a script run as `node <run> <bundle>`
//! instead of running the bundle itself, for library programs whose consumer
//! lives outside the bundle. Node resolves `effect` and the other fixture
//! packages from the repository's root `node_modules`.
//!
//! Byte sizes of both bundles are printed for every program so that the
//! baseline can be read from `cargo test ... -- effect_service_acceptance --nocapture`.

use std::path::{Path, PathBuf};
use std::process::Command;

use rolldown::{
  Bundler, BundlerOptions, ExperimentalOptions, InputItem, IsExternal, OutputFormat, Platform,
};
use serde::Deserialize;

const PROGRAMS: &str =
  concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rolldown/topics/effect_service_acceptance/programs");

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Expect {
  /// Entry module, relative to the program directory. Defaults to `main.js`.
  #[serde(default)]
  entry: Option<String>,
  /// Module specifiers left out of the bundle.
  #[serde(default)]
  external: Vec<String>,
  /// Script run as `node <run> <bundle>` instead of running the bundle itself.
  #[serde(default)]
  run: Option<String>,
  /// Names that must still be in the enabled bundle.
  #[serde(default)]
  keep: Vec<String>,
  /// Names that must not be in the enabled bundle.
  #[serde(default)]
  remove: Vec<String>,
  /// Minimum size reduction of the enabled bundle, in percent.
  #[serde(default)]
  min_reduction_percent: Option<f64>,
  /// Whether the enabled build must emit an `EFFECT_SERVICE_SHAKING` warning.
  #[serde(default)]
  expect_diagnostic: bool,
}

struct Build {
  bundle: PathBuf,
  bytes: u64,
  warnings: Vec<String>,
}

async fn build(dir: &Path, expect: &Expect, enabled: bool) -> Build {
  let out = dir.join("dist").join(if enabled { "on" } else { "off" });
  let _ = std::fs::remove_dir_all(&out);
  let mut bundler = Bundler::new(BundlerOptions {
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: format!("./{}", expect.entry.as_deref().unwrap_or("main.js")),
    }]),
    cwd: Some(dir.to_path_buf()),
    dir: Some(out.to_string_lossy().into_owned()),
    format: Some(OutputFormat::Esm),
    platform: Some(Platform::Node),
    external: (!expect.external.is_empty()).then(|| IsExternal::from(expect.external.clone())),
    experimental: Some(ExperimentalOptions {
      effect_service_shaking: Some(enabled),
      ..Default::default()
    }),
    ..Default::default()
  })
  .expect("failed to create bundler");
  let output = bundler.write().await.expect("build should succeed");
  let bundle = out.join("main.js");
  let bytes = std::fs::metadata(&bundle).expect("bundle should exist").len();
  let warnings = output.warnings.iter().map(|w| format!("{}: {w}", w.kind())).collect();
  Build { bundle, bytes, warnings }
}

fn run(dir: &Path, expect: &Expect, bundle: &Path) -> (i32, String, String) {
  let mut command = Command::new("node");
  command.current_dir(dir);
  match &expect.run {
    Some(script) => command.arg(script).arg(bundle),
    None => command.arg(bundle),
  };
  let output = command.output().expect("node should run");
  (
    output.status.code().unwrap_or(-1),
    String::from_utf8_lossy(&output.stdout).into_owned(),
    String::from_utf8_lossy(&output.stderr).into_owned(),
  )
}

fn word(name: &str) -> regex::Regex {
  regex::Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap()
}

/// Runs the acceptance program in `programs/<name>` and collects every failed
/// expectation, so one run reports everything that is wrong with a program.
async fn check(name: &str) {
  let dir = PathBuf::from(PROGRAMS).join(name);
  let expect: Expect =
    serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).expect("expect.json"))
      .expect("expect.json should match the schema in mod.rs");

  let off = build(&dir, &expect, false).await;
  let on = build(&dir, &expect, true).await;
  let reduction = 100.0 - (on.bytes as f64 / off.bytes as f64) * 100.0;
  println!("{name}: off {} B, on {} B ({reduction:.1}% smaller)", off.bytes, on.bytes);

  let mut failures = vec![];

  let (off_status, off_stdout, off_stderr) = run(&dir, &expect, &off.bundle);
  let (on_status, on_stdout, on_stderr) = run(&dir, &expect, &on.bundle);
  if off_status != 0 {
    failures.push(format!("the program fails with the pass off (exit {off_status}):\n{off_stderr}"));
  }
  if on_status != 0 {
    failures.push(format!("the program fails with the pass on (exit {on_status}):\n{on_stderr}"));
  }
  if off_status == 0 && on_status == 0 && off_stdout != on_stdout {
    failures.push(format!("stdout differs.\noff:\n{off_stdout}\non:\n{on_stdout}"));
  }

  let code = std::fs::read_to_string(&on.bundle).unwrap();
  for keep in &expect.keep {
    if !word(keep).is_match(&code) {
      failures.push(format!("`{keep}` should be kept"));
    }
  }
  for remove in &expect.remove {
    if word(remove).is_match(&code) {
      failures.push(format!("`{remove}` should be removed"));
    }
  }
  if let Some(min) = expect.min_reduction_percent {
    if reduction < min {
      failures.push(format!("the bundle should be at least {min}% smaller, it is {reduction:.1}%"));
    }
  }
  if expect.expect_diagnostic
    && !on.warnings.iter().any(|warning| warning.starts_with("EFFECT_SERVICE_SHAKING"))
  {
    failures.push(format!(
      "an EFFECT_SERVICE_SHAKING warning should be emitted, got: {:#?}",
      on.warnings
    ));
  }

  assert!(failures.is_empty(), "{name}:\n- {}", failures.join("\n- "));
}

/// 1. The real `NodeFileSystem` with only `readFile` used, reached through
/// `yield*`, `Effect.map`/`flatMap`, `Context.get` and `Effect.service`.
#[tokio::test]
async fn node_file_system() {
  check("node_file_system").await;
}

/// 2. The generated OpenAI client with one method used. PR #1 removed 74%.
#[tokio::test]
async fn openai_generated_client() {
  check("openai_generated_client").await;
}

/// 3. A large Distilled client (Stripe) behind an application service, one
/// operation used.
#[tokio::test]
async fn distilled_stripe() {
  check("distilled_stripe").await;
}

/// 4. A library built with `effect` external, consumed by an app that uses
/// every exported key and Layer.
#[tokio::test]
async fn library_effect_external() {
  check("library_effect_external").await;
}

/// 5a. `effect` bundled, an Effect library left external.
#[tokio::test]
async fn mixed_effect_bundled_library_external() {
  check("mixed_effect_bundled_library_external").await;
}

/// 5b. The Effect library bundled, `effect` external.
#[tokio::test]
async fn mixed_library_bundled_effect_external() {
  check("mixed_library_bundled_effect_external").await;
}
