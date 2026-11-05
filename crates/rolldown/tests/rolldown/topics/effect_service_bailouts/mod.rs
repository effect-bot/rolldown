//! Where `experimental.effectServiceShaking` turns itself off, checked per site.
//!
//! Every reason the pass is turned off is an `EFFECT_SERVICE_SHAKING` warning:
//! `id()` is the module of the site, and the message contains `[bailout]` and
//! `at <line>:<col>`, 1-based, not followed by another digit. These tests check
//! that a bailout is present at the expected site, not that it is the only one.
//!
//! The fixtures in `tree_shaking/effect_service_real` check behaviour and that no
//! method is removed separately.

use rolldown::{Bundler, BundlerOptions, ExperimentalOptions, InputItem, Platform};

const FIXTURES: &str =
  concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rolldown/tree_shaking/effect_service_real");

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

/// `None` if bundling `<dir>/main.js` reports a bailout at `needle`, else why not.
async fn missing_bailout(dir: &str, needle: &str) -> Option<String> {
  let reasons = reasons(dir).await;
  let site = position(dir, needle);
  let found = reasons.iter().any(|(id, message)| {
    id.as_deref().is_some_and(|id| id.ends_with(&format!("{dir}/main.js")))
      && message.contains("[bailout]")
      && reports_site(message, &site)
  });
  (!found)
    .then(|| format!("expected a bailout at {dir}/main.js:{site} (`{needle}`), got: {reasons:#?}"))
}

async fn assert_bailout(dir: &str, needle: &str) {
  if let Some(error) = missing_bailout(dir, needle).await {
    panic!("{error}");
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn update_service_scoped_is_a_keyed_lookup() {
  assert_bailout("lookup_update_service_scoped", "Effect.updateServiceScoped(").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn get_or_undefined_unsafe_works_on_key_strings() {
  assert_bailout("string_key_get_or_undefined_unsafe", "Context.getOrUndefinedUnsafe(").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn make_unsafe_works_on_key_strings() {
  assert_bailout("string_key_make_unsafe", "Context.makeUnsafe(").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reexported_loader_loads_code() {
  assert_bailout("loader_reexport", "compile(\"return 1\")").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn computed_host_access_may_load_code() {
  assert_bailout("loader_computed_access", "process[").await;
}

/// A constant computed name for the Function constructor is a code loader.
/// Names known only at run time are outside the contract (A2) and not checked.
#[tokio::test(flavor = "multi_thread")]
async fn constant_function_constructor_name_loads_code() {
  let mut errors = Vec::new();
  for (dir, needle) in [
    ("loader_function_constructor_global", "globalThis.Object["),
    ("loader_function_constructor_arrow", "(() => 0)["),
  ] {
    errors.extend(missing_bailout(dir, needle).await);
  }
  assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reflect_get_with_a_loader_name_loads_code() {
  assert_bailout("loader_reflect_get", "Reflect.get(").await;
}

/// `Reflect.get` with a literal loader name, through `globalThis.Reflect`, an
/// alias of it, or a spread before the key. Rolldown inlines the
/// `const reflect = globalThis.Reflect` alias before the pass runs, so that
/// path reports at the inlined `reflect.get(` call.
#[tokio::test(flavor = "multi_thread")]
async fn qualified_or_spread_reflect_get_loads_code() {
  let mut errors = Vec::new();
  for (dir, needle) in [
    ("loader_reflect_get_global", "globalThis.Reflect.get("),
    ("loader_reflect_get_global", "reflect.get("),
    ("loader_reflect_get_spread", "Reflect.get(...[]"),
  ] {
    errors.extend(missing_bailout(dir, needle).await);
  }
  assert!(errors.is_empty(), "{}", errors.join("\n"));
}
