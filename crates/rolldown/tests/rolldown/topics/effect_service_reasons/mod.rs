//! Why `experimental.effectServiceShaking` keeps methods, checked per site.
//!
//! The pass reports every reason that seeds the build-global heap bit, and every
//! reason it keeps everything, as an `EFFECT_SERVICE_SHAKING` warning:
//! - one warning per distinct (tag, module, position), every reason rather than
//!   only the first;
//! - `id()` is the module that contains the site;
//! - the message contains `[<tag>]` and `at <line>:<col>`, 1-based, of the
//!   start of the write target (or of the call), not followed by another digit.
//!
//! These tests check that a reason is present at the expected site. They don't
//! check that it is the only reason: once the heap bit is set, re-evaluation can
//! add other reasons at the same store.
//!
//! The fixtures are the runtime retention cases in `tree_shaking/effect_service_real`
//! and `tree_shaking/effect_service`, which check behaviour separately.

use rolldown::{Bundler, BundlerOptions, ExperimentalOptions, InputItem, Platform};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rolldown/tree_shaking");

type Reason = (Option<String>, String);

/// The `EFFECT_SERVICE_SHAKING` warnings of bundling `<dir>/main.js`.
async fn reasons(dir: &str) -> Vec<Reason> {
  let mut bundler = Bundler::new(BundlerOptions {
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: "./main.js".to_string(),
    }]),
    cwd: Some(format!("{FIXTURES}/{dir}").into()),
    platform: Some(Platform::Node),
    experimental: Some(ExperimentalOptions {
      effect_service_shaking: Some(true),
      ..Default::default()
    }),
    ..Default::default()
  })
  .expect("failed to create bundler");
  let output = bundler.generate().await.expect("build should succeed");
  output
    .warnings
    .iter()
    .filter(|warning| warning.kind().to_string() == "EFFECT_SERVICE_SHAKING")
    .map(|warning| (warning.id(), warning.to_string()))
    .collect()
}

/// `line:col`, 1-based, of the only occurrence of `needle` in `<dir>/main.js`.
fn position(dir: &str, needle: &str) -> String {
  let source = std::fs::read_to_string(format!("{FIXTURES}/{dir}/main.js")).unwrap();
  let mut matches = source.match_indices(needle);
  let (offset, _) = matches.next().unwrap_or_else(|| panic!("`{needle}` not in {dir}/main.js"));
  assert!(matches.next().is_none(), "`{needle}` occurs more than once in {dir}/main.js");
  let before = &source[..offset];
  let line = before.matches('\n').count() + 1;
  let column = before.len() - before.rfind('\n').map_or(0, |newline| newline + 1) + 1;
  format!("{line}:{column}")
}

/// Whether `message` reports `at <site>` with no further digit after it.
fn reports_site(message: &str, site: &str) -> bool {
  let needle = format!("at {site}");
  message
    .match_indices(&needle)
    .any(|(offset, _)| !message[offset + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

fn assert_reason(reasons: &[Reason], dir: &str, tag: &str, needle: &str) {
  let site = position(dir, needle);
  let found = reasons.iter().any(|(id, message)| {
    id.as_deref().is_some_and(|id| id.ends_with(&format!("{dir}/main.js")))
      && message.contains(&format!("[{tag}]"))
      && reports_site(message, &site)
  });
  assert!(
    found,
    "expected a `[{tag}]` reason at {dir}/main.js:{site} (`{needle}`), got: {reasons:#?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn tainted_payload_store_seeds_heap() {
  let dir = "effect_service_real/mutated_exit_join";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:stored-value", r#"fiber._exit["~effect/Effect/args"]"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_into_runtime_object_seeds_heap() {
  let dir = "effect_service_real/completion_replacement";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:receiver-runtime", "fiber._exit =");
}

#[tokio::test(flavor = "multi_thread")]
async fn write_through_called_helper_seeds_heap() {
  let dir = "effect_service_real/completion_replacement_helper";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:receiver-runtime", "target._exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn write_through_escaped_helper_seeds_heap() {
  let dir = "effect_service_real/completion_replacement_escaped_helper";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:receiver-unresolved", "target._exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_assignment_seeds_heap() {
  let dir = "effect_service_real/hook_assignment";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:hook-name", "box.toJSON");
}

#[tokio::test(flavor = "multi_thread")]
async fn version_mismatch_is_reported() {
  let reasons = reasons("effect_service/version_mismatch").await;
  let found = reasons
    .iter()
    .any(|(_, message)| message.contains("[version-mismatch]") && message.contains("4.0.0"));
  assert!(found, "expected a `[version-mismatch]` reason naming 4.0.0, got: {reasons:#?}");
}

/// No reason at all at the site of `needle`: the call there matched its row.
fn assert_no_reason_at(reasons: &[Reason], dir: &str, needle: &str) {
  let site = position(dir, needle);
  let found: Vec<&Reason> = reasons
    .iter()
    .filter(|(id, message)| {
      id.as_deref().is_some_and(|id| id.ends_with(&format!("{dir}/main.js")))
        && reports_site(message, &site)
    })
    .collect();
  assert!(
    found.is_empty(),
    "expected no reason at {dir}/main.js:{site} (`{needle}`), got: {found:#?}"
  );
}

/// No reason seeds the heap bit, so nothing unrelated keeps the methods.
fn assert_heap_clear(reasons: &[Reason], dir: &str) {
  let found: Vec<&Reason> =
    reasons.iter().filter(|(_, message)| message.contains("[heap:")).collect();
  assert!(found.is_empty(), "expected no heap reason in {dir}, got: {found:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn map_success_binding_reaches_the_sink() {
  let dir = "effect_service_real/map_success_binding";
  let reasons = reasons(dir).await;
  assert_heap_clear(&reasons, dir);
  assert_no_reason_at(&reasons, dir, "Effect.map(");
  assert_no_reason_at(&reasons, dir, "Effect.forkChild(");
  assert_no_reason_at(&reasons, dir, "Effect.sync(() => 0)");
  assert_reason(&reasons, dir, "bailout", "JSON.stringify(value, replacer)");
}

#[tokio::test(flavor = "multi_thread")]
async fn curried_map_is_an_unsummarised_call() {
  let dir = "effect_service_real/map_curried_fallback";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:unsummarised-call", "Effect.map((value) => value)");
}

#[tokio::test(flavor = "multi_thread")]
async fn record_all_is_an_unsummarised_call() {
  let dir = "effect_service_real/all_record_fallback";
  let reasons = reasons(dir).await;
  assert_reason(&reasons, dir, "heap:unsummarised-call", "Effect.all(");
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_origin_join_reaches_the_sink() {
  let dir = "effect_service_real/all_mixed_origin_join";
  let reasons = reasons(dir).await;
  assert_heap_clear(&reasons, dir);
  assert_no_reason_at(&reasons, dir, "Effect.all(");
  assert_no_reason_at(&reasons, dir, "Fiber.join(");
  assert_no_reason_at(&reasons, dir, "Effect.forkChild(leak)");
  assert_reason(&reasons, dir, "bailout", "JSON.stringify(value, replacer)");
}
