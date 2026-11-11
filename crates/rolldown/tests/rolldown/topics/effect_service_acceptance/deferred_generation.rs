//! Deferred generation against eager generation (implementation.md, "Deferred
//! generation"). Each program is built twice in this process, with
//! `experimental.effectServiceShakingEagerGeneration` and without it; both
//! builds must emit the same bundle and make the same decisions. Both builds
//! run the same head, so this compares the two generation modes, not this head
//! with an earlier one.
//!
//! The pass writes its decision record as a debug event (`record.rs`):
//! bail reasons, keys, removals, widening, which lazy statements the solve
//! activated, the literals and whether each is a key's value, and per body and
//! per object the values that flowed and the escapes. Decision lines must
//! match, counting repeats.
//!
//! Lines starting with `dormant` describe what generating a statement that
//! never runs records (its literals, its class holes): eager generation has
//! them for every such statement, deferred generation only for those it
//! generates up front. Each deferred dormant line must be among the eager ones
//! (counting repeats), and every eager one must show it took part in nothing:
//! no property removed, only variables linked to nothing hold it, it is no
//! key's value, it has no demand and it did not escape.
//!
//! Body flows, key providers and escapes are supporting evidence. The record
//! names objects by kind and site, so it is lossy (`record.rs`) and not an
//! equivalence oracle. For the deferred-generation guards (`STRICT`) they must
//! match line for line. Elsewhere a difference is printed and has to be
//! explained (implementation.md lists the known ones).

use std::path::{Path, PathBuf};

use rolldown::{Bundler, BundlerOptions, ExperimentalOptions, InputItem, IsExternal, OutputFormat};

use super::{PROGRAMS, solve_reports};

const FIXTURES: &str =
  concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rolldown/tree_shaking/effect_service");

/// One build's output: its chunks and assets by file name, its decision record
/// and the fields of its `effect service shaking` event.
struct Built {
  files: Vec<(String, Vec<u8>)>,
  record: String,
  report: Vec<(String, String)>,
}

async fn build(options: &BundlerOptions, eager: bool) -> Built {
  let mut options = options.clone();
  let experimental = options.experimental.get_or_insert_with(ExperimentalOptions::default);
  experimental.effect_service_shaking_eager_generation = eager.then_some(true);
  let thread = std::thread::current().id();
  let reports = solve_reports();
  let output = Bundler::new(options)
    .expect("failed to create bundler")
    .generate()
    .await
    .expect("build should succeed");
  let mut files: Vec<(String, Vec<u8>)> = output
    .assets
    .iter()
    .map(|o| (o.filename().to_string(), o.content_as_bytes().to_vec()))
    .collect();
  files.sort();
  let record = {
    let mut records = reports.1.lock().unwrap();
    let at = records.iter().rposition(|(id, _)| *id == thread);
    records.remove(at.expect("the build should record its decisions")).1
  };
  let report = {
    let mut events = reports.0.lock().unwrap();
    let at = events.iter().rposition(|(id, _)| *id == thread);
    events.remove(at.expect("the build should report its solve")).1
  };
  Built { files, record, report }
}

/// Decision lines, the rest (body flows and escapes, keyed by their body), and
/// dormant lines. A solve that ran out of budget stopped part way: what it had
/// activated by then is intermediate too, and only the outcome (every method
/// kept, the reasons, the keys) is a decision.
fn split(record: &str) -> (Vec<&str>, Vec<String>, Vec<&str>) {
  let (mut decisions, mut intermediate, mut dormant) = (vec![], vec![], vec![]);
  let stopped =
    record.lines().any(|l| l.starts_with("bail ") && l.contains("exceeded its allocation budget"));
  let outcome = ["solves ", "bail_all ", "bail ", "never_prune ", "key ", "removed ", "widened "];
  let mut head = "";
  for line in record.lines() {
    if stopped && !outcome.iter().any(|p| line.starts_with(p)) {
      if !line.starts_with("  ") {
        head = line;
      }
      intermediate.push(format!("{head} || {line}"));
    } else if line.starts_with("dormant ") {
      dormant.push(line);
    } else if line.starts_with("  ") {
      intermediate.push(format!("{head} || {line}"));
    } else if line.starts_with("body ")
      || line.starts_with("escaped ")
      || line.starts_with("class escaped ")
      || line.starts_with("provider ")
    {
      head = line;
      intermediate.push(line.to_string());
    } else if !line.starts_with("bail rss ") {
      // The budget stop's histogram counts what was generated, an accounting figure.
      decisions.push(line);
    }
  }
  (decisions, intermediate, dormant)
}

/// The lines only `a` has and those only `b` has, counting repeats.
fn difference<'l, L: AsRef<str>>(a: &'l [L], b: &'l [L]) -> (Vec<&'l str>, Vec<&'l str>) {
  let mut counts: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
  for l in a {
    *counts.entry(l.as_ref()).or_default() += 1;
  }
  for l in b {
    *counts.entry(l.as_ref()).or_default() -= 1;
  }
  let (mut only_a, mut only_b) = (vec![], vec![]);
  for (l, n) in counts {
    for _ in 0..n.max(0) {
      only_a.push(l);
    }
    for _ in 0..(-n).max(0) {
      only_b.push(l);
    }
  }
  (only_a, only_b)
}

fn field<'a>(built: &'a Built, name: &str) -> &'a str {
  built.report.iter().find(|(n, _)| n == name).map_or("", |(_, v)| v.as_str())
}

/// The fixture's options as the fixture harness applies them (`IntegrationTest`).
fn fixture_options(dir: &Path) -> Option<BundlerOptions> {
  let config = rolldown_testing::test_config::read_test_config(&dir.join("_config.json"));
  let mut options = config.config;
  let experimental = options.experimental.as_mut()?;
  if !experimental.is_effect_service_shaking_enabled() {
    return None;
  }
  experimental
    .effect_service_shaking_max_entries
    .get_or_insert(rolldown_testing::effect_fixture::MAX_ENTRIES);
  options.cwd.get_or_insert_with(|| dir.to_path_buf());
  options.external.get_or_insert_with(|| {
    IsExternal::from(vec!["node:assert".to_string(), "node:assert/strict".to_string()])
  });
  options.input.get_or_insert_with(|| {
    vec![InputItem { name: Some("main".to_string()), import: "./main.js".to_string() }]
  });
  Some(options)
}

fn program_options(dir: &Path) -> BundlerOptions {
  let expect: super::Expect = std::fs::read_to_string(dir.join("expect.json"))
    .map(|s| serde_json::from_str(&s).expect("expect.json should parse"))
    .unwrap_or_default();
  BundlerOptions {
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: "./main.js".to_string(),
    }]),
    cwd: Some(dir.to_path_buf()),
    format: Some(OutputFormat::Esm),
    platform: Some(rolldown::Platform::Node),
    external: (!expect.external.is_empty()).then(|| IsExternal::from(expect.external.clone())),
    experimental: Some(ExperimentalOptions {
      effect_service_shaking: Some(true),
      attach_debug_info: Some(rolldown::AttachDebugInfo::None),
      ..Default::default()
    }),
    ..Default::default()
  }
}

/// The guards whose flows and escapes must match too, line for line.
const STRICT: [&str; 4] = [
  "deferred_activation_paths",
  "deferred_dormant_eager_facts",
  "deferred_dormant_unsafe_lookup",
  "deferred_late_arrivals",
];

/// The acceptance programs, the deferred-generation guard fixtures and the
/// fixtures that refine module or class holes; every `tree_shaking/effect_service`
/// fixture with `ROLLDOWN_TEST_DEFERRED_GENERATION_ALL` set.
#[tokio::test]
#[expect(
  clippy::print_stderr,
  reason = "the differences the test tolerates are printed for the reader"
)]
async fn deferred_generation_matches_eager() {
  let _memory_guard = rolldown_testing::memory_guard::guard(
    "effect_service_acceptance::deferred_generation_matches_eager",
  );
  if std::env::var_os("ROLLDOWN_EFFECT_SERVICE_SHAKING_EAGER_GENERATION").is_some() {
    // The variable makes both builds eager: there is nothing to compare.
    eprintln!(
      "deferred_generation_matches_eager: skipped, ROLLDOWN_EFFECT_SERVICE_SHAKING_EAGER_GENERATION is set"
    );
    return;
  }
  let mut cases: Vec<(String, BundlerOptions)> = vec![];
  for name in ["node_file_system", "ordinary_app", "openai_generated_client", "distilled_stripe"] {
    cases.push((name.to_string(), program_options(&PathBuf::from(PROGRAMS).join(name))));
  }
  let mut fixtures: Vec<PathBuf> = std::fs::read_dir(FIXTURES)
    .expect("the fixtures should be listed")
    .map(|e| e.expect("a fixture entry").path())
    .filter(|p| p.join("_config.json").exists())
    .filter(|p| {
      let name = p.file_name().unwrap().to_string_lossy();
      std::env::var_os("ROLLDOWN_TEST_DEFERRED_GENERATION_ALL").is_some()
        || ["deferred_", "module_hole_", "class_hole_"]
          .iter()
          .any(|prefix| name.starts_with(prefix))
    })
    .collect();
  fixtures.sort();
  for dir in fixtures {
    if let Some(options) = fixture_options(&dir) {
      let name = dir.file_name().unwrap().to_string_lossy().into_owned();
      cases.push((name, options));
    }
  }

  let mut failures: Vec<String> = vec![];
  let mut intermediate: Vec<String> = vec![];
  let mut records: Vec<(String, Built, Built)> = vec![];
  for (name, options) in cases {
    let eager = build(&options, true).await;
    let deferred = build(&options, false).await;
    if eager.files != deferred.files {
      failures.push(format!("{name}: the bundles differ"));
    }
    let (eager_decisions, eager_rest, eager_dormant) = split(&eager.record);
    let (deferred_decisions, deferred_rest, deferred_dormant) = split(&deferred.record);
    if let Some(dir) = std::env::var_os("ROLLDOWN_TEST_DEFERRED_GENERATION_DUMP") {
      let dir = PathBuf::from(dir);
      std::fs::create_dir_all(&dir).expect("the dump directory");
      std::fs::write(dir.join(format!("{name}.eager")), &eager.record).expect("dump");
      std::fs::write(dir.join(format!("{name}.deferred")), &deferred.record).expect("dump");
    }
    let (eager_only, deferred_only) = difference(&eager_decisions, &deferred_decisions);
    if !eager_only.is_empty() || !deferred_only.is_empty() {
      failures.push(format!(
        "{name}: the decisions differ; eager only {:?}, deferred only {:?}",
        &eager_only[..eager_only.len().min(4)],
        &deferred_only[..deferred_only.len().min(4)]
      ));
    }
    // Deferred generation records a dormant fact only for a statement it generates up front.
    let (_, deferred_only) = difference(&eager_dormant, &deferred_dormant);
    for line in deferred_only {
      failures.push(format!("{name}: deferred generation records {line:?}, eager does not"));
    }
    // What eager generation makes for a statement no solve reaches takes part in nothing: only
    // variables linked to nothing hold it (`record.rs`), it is no key's value, has no demand
    // and did not escape.
    for line in &eager_dormant {
      let inert = if line.starts_with("dormant literal ") {
        line.contains(" removed=0 ")
          && line.ends_with(" linked=0 candidate=false demand=none escaped=false")
      } else {
        line.contains(" linked=0 x")
      };
      if !inert {
        failures.push(format!("{name}: a dormant fact takes part: {line}"));
      }
    }
    let (eager_only, deferred_only) = difference(&eager_rest, &deferred_rest);
    let differing = eager_only.len() + deferred_only.len();
    if differing > 0 {
      if STRICT.contains(&name.as_str()) {
        failures.push(format!(
          "{name}: flows or escapes differ; eager only {:?}, deferred only {:?}",
          &eager_only[..eager_only.len().min(4)],
          &deferred_only[..deferred_only.len().min(4)]
        ));
      }
      intermediate.push(format!("{name} ({differing} lines)"));
    }
    if field(&deferred, "generated").parse::<usize>().unwrap_or(0)
      > field(&deferred, "deferred").parse::<usize>().unwrap_or(0)
    {
      failures.push(format!("{name}: more statements generated than deferred"));
    }
    records.push((name, eager, deferred));
  }

  let find = |name: &str| records.iter().find(|(n, _, _)| n == name).expect("a guard fixture");
  // Guard: every path reaches its statement, generated once (`debug_assert` in the pass).
  let (_, _, deferred) = find("deferred_activation_paths");
  assert!(
    deferred
      .record
      .lines()
      .any(|l| l.starts_with("lazy ") && l.contains("other.js") && l.ends_with("activated=false")),
    "other.js's `neverReached` should stay dormant"
  );
  assert_eq!(
    deferred
      .record
      .lines()
      .filter(|l| l.starts_with("lazy ") && l.contains("/lib.js:"))
      .filter(|l| l.ends_with("activated=false"))
      .count(),
    0,
    "every lazy statement of lib.js is reached"
  );
  // Guard: a dormant class that bails bails the same way under both.
  let (_, eager, deferred) = find("deferred_dormant_unsafe_lookup");
  for built in [eager, deferred] {
    assert!(
      built.record.lines().any(|l| l == "bail_all true"),
      "the unsafe lookup keeps everything"
    );
    assert!(built.record.lines().any(|l| l.contains("references Context.getOrUndefinedUnsafe")));
  }
  // Guard: the budget runs out generating a deferred statement, part way through its one
  // initializer: generation stops there and the rest of the table is not generated. Eager
  // generation completes under the budget and the solve stops before its first step.
  let (_, eager, deferred) = find("deferred_generation_budget");
  assert_eq!(field(deferred, "budget_exceeded_generating"), "true");
  assert_eq!(field(eager, "budget_exceeded_generating"), "false");
  assert_eq!(field(eager, "generation_stopped"), "");
  assert_eq!(field(eager, "steps"), "0");
  let lib = Path::new(FIXTURES).join("deferred_generation_budget/lib.js");
  let source = std::fs::read_to_string(&lib).expect("the fixture's lib.js");
  let first = source.find("entry0:").expect("the table's first member") as u64;
  let last = source.find("entry1999:").expect("the table's last member") as u64;
  let stopped = field(deferred, "generation_stopped");
  eprintln!("deferred generation: the budget ran out generating {stopped}, table {first}..{last}");
  let (module, at) = stopped.rsplit_once(':').expect("generation stopped part way");
  assert!(module.ends_with("deferred_generation_budget/lib.js"), "stopped in {stopped}");
  let at: u64 = at.parse().expect("an offset");
  assert!(first < at && at < last, "generation should stop inside the table, at {at}");
  // Guard: both solves of a refinement generate on activation, each in a fresh analysis.
  let refined = records
    .iter()
    .filter(|(_, _, d)| d.record.starts_with("solves 2") && field(d, "generated") != "0")
    .count();
  assert!(refined > 0, "some fixture should refine with deferred statements generated");

  eprintln!("deferred generation: intermediate results differ in {intermediate:?}");
  assert!(failures.is_empty(), "deferred generation:\n- {}", failures.join("\n- "));
}
