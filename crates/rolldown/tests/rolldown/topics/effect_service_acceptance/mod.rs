//! Acceptance set for `experimental.effectServiceShaking` (EFF-3430).
//!
//! Each program under `programs/<name>/` is bundled twice, with the pass off
//! and on, and both bundles are run with Node. A program passes when:
//!
//! - both runs exit 0 with identical stdout (behaviour is unchanged);
//! - every name in `expect.json#keep` is still in the enabled bundle;
//! - no name in `expect.json#remove` is in the enabled bundle;
//! - each name in `expect.json#removeProperties` has fewer object-literal
//!   definitions in the enabled bundle, even if unrelated uses keep the name;
//! - names are matched as whole words in the bundle's code with every comment
//!   blanked out, so a name that survives only in a JSDoc example neither
//!   satisfies `keep` nor fails `remove`; string literals still count;
//! - the enabled bundle is at least `minReductionPercent` smaller, when set;
//! - an `EFFECT_SERVICE_SHAKING` warning is emitted, when `expectDiagnostic` is set.
//!
//! Both builds set `experimental.attachDebugInfo: "none"`: the module-path
//! comments otherwise depend on where the packages are installed (pnpm's
//! store paths), so sizes would not compare across checkouts.
//!
//! The two bundles are written to `dist/off/main.js` and `dist/on/main.js` in
//! the program directory. Node resolves `effect` and the other fixture
//! packages from the repository's root `node_modules`.
//!
//! Byte sizes of both bundles are printed for every program so that the
//! baseline can be read from `cargo test ... -- effect_service_acceptance --nocapture`.
//!
//! `solve_is_deterministic` builds one program several times into
//! `dist/deterministic` and compares what the pass reports about each solve
//! (see its doc comment).
//!
//! The builds run in the test process, so every program runs under
//! `rolldown_testing::memory_guard`. If the analysis runs away, the process
//! stops at `ROLLDOWN_TEST_MAX_RSS_MB` (6 GiB by default) instead of
//! exhausting the host.

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
  /// Module specifiers left out of the bundle.
  #[serde(default)]
  external: Vec<String>,
  /// Names that must still be in the enabled bundle.
  #[serde(default)]
  keep: Vec<String>,
  /// Names that must not be in the enabled bundle.
  #[serde(default)]
  remove: Vec<String>,
  /// Object-literal property names whose definition count must decrease.
  #[serde(default)]
  remove_properties: Vec<String>,
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

/// Bundles the program into `dist/<out>`.
async fn build(dir: &Path, expect: &Expect, enabled: bool, out: &str) -> Build {
  let out = dir.join("dist").join(out);
  let _ = std::fs::remove_dir_all(&out);
  let mut bundler = Bundler::new(BundlerOptions {
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: "./main.js".to_string(),
    }]),
    cwd: Some(dir.to_path_buf()),
    dir: Some(out.to_string_lossy().into_owned()),
    format: Some(OutputFormat::Esm),
    platform: Some(Platform::Node),
    external: (!expect.external.is_empty()).then(|| IsExternal::from(expect.external.clone())),
    experimental: Some(ExperimentalOptions {
      effect_service_shaking: Some(enabled),
      // Module-path comments depend on the installation layout (pnpm's store paths), not
      // on the code: without them, sizes compare across checkouts.
      attach_debug_info: Some(rolldown::AttachDebugInfo::None),
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

fn run(dir: &Path, bundle: &Path) -> (i32, String, String) {
  let output = Command::new("node").current_dir(dir).arg(bundle).output().expect("node should run");
  (
    output.status.code().unwrap_or(-1),
    String::from_utf8_lossy(&output.stdout).into_owned(),
    String::from_utf8_lossy(&output.stderr).into_owned(),
  )
}

fn word(name: &str) -> regex::Regex {
  regex::Regex::new(&format!(r"\b{}\b", regex::escape(name))).unwrap()
}

/// The bundle with every comment replaced by a space. Code and string
/// literals are untouched, so a name still present anywhere but a comment is
/// still found.
fn without_comments(code: &str) -> String {
  let allocator = oxc::allocator::Allocator::default();
  let ret = oxc::parser::Parser::new(&allocator, code, oxc::span::SourceType::mjs()).parse();
  assert!(
    !ret.fatal_error && ret.diagnostics.is_empty(),
    "the enabled bundle should parse: {:?}",
    ret.diagnostics
  );
  let mut stripped = String::with_capacity(code.len());
  let mut end = 0;
  for comment in &ret.program.comments {
    stripped.push_str(&code[end..comment.span.start as usize]);
    stripped.push(' ');
    end = comment.span.end as usize;
  }
  stripped.push_str(&code[end..]);
  stripped
}

/// Count definitions, not reads or strings, so common names such as `remove`
/// can guard pruning without rejecting unrelated code that uses the same name.
fn property_counts(code: &str) -> std::collections::BTreeMap<String, usize> {
  use oxc::ast::ast::ObjectProperty;
  use oxc::ast_visit::{Visit, walk};

  #[derive(Default)]
  struct Properties(std::collections::BTreeMap<String, usize>);

  impl<'a> Visit<'a> for Properties {
    fn visit_object_property(&mut self, property: &ObjectProperty<'a>) {
      if let Some(name) =
        property.key.static_name().and_then(oxc::ast::StaticPropertyName::into_cow_str)
      {
        *self.0.entry(name.into_owned()).or_default() += 1;
      }
      walk::walk_object_property(self, property);
    }
  }

  let allocator = oxc::allocator::Allocator::default();
  let ret = oxc::parser::Parser::new(&allocator, code, oxc::span::SourceType::mjs()).parse();
  assert!(
    !ret.fatal_error && ret.diagnostics.is_empty(),
    "the bundle should parse: {:?}",
    ret.diagnostics
  );
  let mut properties = Properties::default();
  properties.visit_program(&ret.program);
  properties.0
}

/// Runs the acceptance program in `programs/<name>` and collects every failed
/// expectation, so one run reports everything that is wrong with a program.
async fn check(name: &str) {
  let _memory_guard =
    rolldown_testing::memory_guard::guard(format!("effect_service_acceptance::{name}"));
  let dir = PathBuf::from(PROGRAMS).join(name);
  let expect: Expect =
    serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).expect("expect.json"))
      .expect("expect.json should match the schema in mod.rs");

  let off = build(&dir, &expect, false, "off").await;
  let on = build(&dir, &expect, true, "on").await;
  #[expect(clippy::cast_precision_loss, reason = "bundle sizes are far below 2^52 bytes")]
  let reduction = (on.bytes as f64 / off.bytes as f64).mul_add(-100.0, 100.0);
  #[expect(clippy::print_stdout, reason = "the size report is the point of the run")]
  {
    println!("{name}: off {} B, on {} B ({reduction:.1}% smaller)", off.bytes, on.bytes);
  }

  let mut failures = vec![];

  let (off_status, off_stdout, off_stderr) = run(&dir, &off.bundle);
  let (on_status, on_stdout, on_stderr) = run(&dir, &on.bundle);
  if off_status != 0 {
    failures
      .push(format!("the program fails with the pass off (exit {off_status}):\n{off_stderr}"));
  }
  if on_status != 0 {
    failures.push(format!("the program fails with the pass on (exit {on_status}):\n{on_stderr}"));
  }
  if off_status == 0 && on_status == 0 && off_stdout != on_stdout {
    failures.push(format!("stdout differs.\noff:\n{off_stdout}\non:\n{on_stdout}"));
  }

  let code = without_comments(&std::fs::read_to_string(&on.bundle).unwrap());
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
  if !expect.remove_properties.is_empty() {
    let off_properties = property_counts(&std::fs::read_to_string(&off.bundle).unwrap());
    let on_properties = property_counts(&code);
    for name in &expect.remove_properties {
      let off_count = off_properties.get(name).copied().unwrap_or_default();
      let on_count = on_properties.get(name).copied().unwrap_or_default();
      if on_count >= off_count {
        failures.push(format!(
          "`{name}` should lose an object-literal definition (off {off_count}, on {on_count})"
        ));
      }
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
/// The 31 unused property names are pinned in `expect.json`, including names
/// that survive elsewhere in the bundle. Type-modelled Effect modules must
/// preserve the constructor's pruning.
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

/// 4a. `effect` bundled, an Effect library left external.
#[tokio::test]
async fn mixed_effect_bundled_library_external() {
  check("mixed_effect_bundled_library_external").await;
}

/// 4b. The Effect library bundled, `effect` external. This is also the
/// application build with `effect` external: it must behave the same and emit
/// the diagnostic; pruning is allowed but not required, so nothing is in `remove`.
#[tokio::test]
async fn mixed_library_bundled_effect_external() {
  check("mixed_library_bundled_effect_external").await;
}

/// What the copy of `effect` in `declarations` has beside each module.
#[derive(Clone, Copy)]
enum Declarations {
  /// No `.d.ts` files.
  Missing,
  /// `.d.ts` files that are not UTF-8, so they can't be read.
  Unreadable,
}

/// The installed `effect` package, found as Node finds it from the programs.
fn installed_effect() -> PathBuf {
  Path::new(PROGRAMS)
    .ancestors()
    .map(|dir| dir.join("node_modules/effect"))
    .find(|dir| dir.join("package.json").is_file())
    .and_then(|dir| dir.canonicalize().ok())
    .expect("the repository should install effect")
}

/// Copies `effect`'s `package.json` and every `.js` module of `src` into `dst`,
/// with the declarations each module gets.
fn copy_effect(src: &Path, dst: &Path, declarations: Declarations) {
  std::fs::create_dir_all(dst).unwrap();
  for entry in std::fs::read_dir(src).unwrap() {
    let path = entry.unwrap().path();
    let target = dst.join(path.file_name().unwrap());
    if path.is_dir() {
      copy_effect(&path, &target, declarations);
    } else if path.extension().is_some_and(|e| e == "js") {
      if std::fs::hard_link(&path, &target).is_err() {
        std::fs::copy(&path, &target).unwrap();
      }
      if let Declarations::Unreadable = declarations {
        std::fs::write(target.with_extension("d.ts"), b"export declare const \xff\xfe: never;\n")
          .unwrap();
      }
    }
  }
}

/// `effect`'s declarations can't be found or read: the pass keeps every method
/// and says why, rather than analysing `effect`'s modules some other way
/// (issue, "Analysis model"). The program is built in a temporary directory
/// whose `node_modules/effect` is a copy of the installed package's
/// `package.json` and `.js` modules.
async fn check_declarations(name: &str, declarations: Declarations) {
  let _memory_guard =
    rolldown_testing::memory_guard::guard(format!("effect_service_acceptance::{name}"));
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("effect_service_acceptance").join(name);
  let _ = std::fs::remove_dir_all(&dir);
  let effect = installed_effect();
  let copy = dir.join("node_modules/effect");
  std::fs::create_dir_all(&copy).unwrap();
  std::fs::copy(effect.join("package.json"), copy.join("package.json")).unwrap();
  copy_effect(&effect.join("dist"), &copy.join("dist"), declarations);
  std::fs::write(dir.join("package.json"), "{ \"type\": \"module\" }\n").unwrap();
  std::fs::copy(PathBuf::from(PROGRAMS).join("declarations/main.js"), dir.join("main.js")).unwrap();

  let expect = Expect::default();
  let off = build(&dir, &expect, false, "off").await;
  let on = build(&dir, &expect, true, "on").await;
  let mut failures = vec![];
  let (off_status, off_stdout, off_stderr) = run(&dir, &off.bundle);
  let (on_status, on_stdout, on_stderr) = run(&dir, &on.bundle);
  if off_status != 0 || on_status != 0 || off_stdout != on_stdout {
    failures.push(format!(
      "both builds should run alike: off exit {off_status} {off_stdout}{off_stderr}, on exit {on_status} {on_stdout}{on_stderr}"
    ));
  }
  let code = without_comments(&std::fs::read_to_string(&on.bundle).unwrap());
  for method in ["declarationsUsed", "declarationsUnused"] {
    if !regex::Regex::new(&format!(r"\b{method}\s*:")).unwrap().is_match(&code) {
      failures.push(format!("the definition of `{method}` should be kept"));
    }
  }
  if !on.warnings.iter().any(|w| w.starts_with("EFFECT_SERVICE_SHAKING") && w.contains(".d.ts")) {
    failures.push(format!(
      "an EFFECT_SERVICE_SHAKING warning should name the declarations, got: {:#?}",
      on.warnings
    ));
  }
  assert!(failures.is_empty(), "{name}:\n- {}", failures.join("\n- "));
}

/// `effect` installed without its `.d.ts` files.
#[tokio::test]
async fn missing_declarations() {
  check_declarations("missing_declarations", Declarations::Missing).await;
}

/// `effect` installed with `.d.ts` files that can't be read.
#[tokio::test]
async fn unreadable_declarations() {
  check_declarations("unreadable_declarations", Declarations::Unreadable).await;
}

/// The fields of the pass's `effect service shaking` debug event, except the
/// timings: what the solve produced (`vars`, `objs`, `steps`, `removed`).
type Report = Vec<(String, String)>;

/// Every solve reported in the process, with the thread its build ran on.
#[derive(Default)]
struct SolveReports(std::sync::Mutex<Vec<(std::thread::ThreadId, Report)>>);

struct Fields(Report);

impl tracing::field::Visit for Fields {
  fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
    self.0.push((field.name().to_string(), format!("{value:?}")));
  }
}

impl tracing::Subscriber for SolveReports {
  fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
    metadata.target().contains("effect_service_shaking")
  }
  fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }
  fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
  fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
  fn event(&self, event: &tracing::Event<'_>) {
    let mut fields = Fields(vec![]);
    event.record(&mut fields);
    if fields.0.iter().any(|(name, value)| name == "message" && value == "effect service shaking") {
      fields.0.retain(|(name, _)| !matches!(name.as_str(), "message" | "solve_ms" | "total_ms"));
      self.0.lock().unwrap().push((std::thread::current().id(), fields.0));
    }
  }
  fn enter(&self, _: &tracing::span::Id) {}
  fn exit(&self, _: &tracing::span::Id) {}
}

/// The process-wide recorder of solves. It is the global subscriber: a
/// subscriber scoped to the test thread misses the event whenever another test
/// thread reaches the pass first, because `tracing` then caches the event as
/// disabled.
fn solve_reports() -> &'static std::sync::Arc<SolveReports> {
  static REPORTS: std::sync::OnceLock<std::sync::Arc<SolveReports>> = std::sync::OnceLock::new();
  REPORTS.get_or_init(|| {
    let reports = std::sync::Arc::new(SolveReports::default());
    tracing::subscriber::set_global_default(std::sync::Arc::clone(&reports))
      .expect("no other global tracing subscriber should be set");
    reports
  })
}

/// The same input gives the same analysis. `node_file_system` is bundled
/// several times with the pass on: every build must emit the same bundle and
/// the same solve (variables, objects, solver steps, removals). A solve that
/// depends on module load order flips programs near the allocation budget
/// between removing methods and keeping everything.
#[tokio::test]
async fn solve_is_deterministic() {
  const BUILDS: usize = 5;
  let _memory_guard =
    rolldown_testing::memory_guard::guard("effect_service_acceptance::solve_is_deterministic");
  let dir = PathBuf::from(PROGRAMS).join("node_file_system");
  let expect = Expect::default();
  let reports = solve_reports();
  // The builds run on this thread; other tests may build at the same time.
  let thread = std::thread::current().id();
  let mut bundles = vec![];
  for _ in 0..BUILDS {
    let on = build(&dir, &expect, true, "deterministic").await;
    bundles.push(std::fs::read_to_string(&on.bundle).unwrap());
  }
  let reports: Vec<_> = reports
    .0
    .lock()
    .unwrap()
    .iter()
    .filter(|(id, _)| *id == thread)
    .map(|(_, report)| report.clone())
    .collect();
  assert_eq!(reports.len(), BUILDS, "every build should report one solve");
  for (i, bundle) in bundles.iter().enumerate().skip(1) {
    assert!(bundle == &bundles[0], "build {i} emits a different bundle than build 0");
  }
  for (i, report) in reports.iter().enumerate().skip(1) {
    assert_eq!(report, &reports[0], "build {i} solves differently than build 0");
  }
}
