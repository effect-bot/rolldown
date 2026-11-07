//! A plugin that panics in `transform` must end the build, not hang it.
//!
//! The module loader runs each module task with `tokio::spawn` and waits for their messages
//! on a channel whose sender it also holds, so a task that panics never reports back and the
//! loader waits forever. The build runs under a timeout so that hang fails this test instead
//! of the suite. Minification and tree-shaking play no part.

use std::{borrow::Cow, path::Path, time::Duration};

use rolldown::{Bundler, BundlerOptions, InputItem};
use rolldown_plugin::{
  HookTransformArgs, HookTransformReturn, HookUsage, Plugin, SharedTransformPluginContext,
};

#[derive(Debug)]
struct PanicInTransform;

impl Plugin for PanicInTransform {
  fn name(&self) -> Cow<'static, str> {
    "panic-in-transform".into()
  }

  fn register_hook_usage(&self) -> HookUsage {
    HookUsage::Transform
  }

  async fn transform(
    &self,
    _ctx: SharedTransformPluginContext,
    _args: &HookTransformArgs<'_>,
  ) -> HookTransformReturn {
    panic!("panic-in-transform: deliberate panic");
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_panic_in_transform_ends_the_build() {
  let cwd =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rolldown/errors/plugin_transform_panic");
  let mut bundler = Bundler::with_plugins(
    BundlerOptions {
      input: Some(vec![InputItem {
        name: Some("entry".to_string()),
        import: "./entry.js".to_string(),
      }]),
      cwd: Some(cwd),
      ..Default::default()
    },
    vec![Plugin::new_shared(PanicInTransform)],
  )
  .expect("the options are valid");
  let build = tokio::spawn(async move { bundler.generate().await.map(|_| ()) });
  match tokio::time::timeout(Duration::from_secs(10), build).await {
    Err(elapsed) => {
      panic!("the build did not end after a plugin panicked in `transform`: {elapsed}")
    }
    // Re-raising the plugin's panic ends the build too.
    Ok(Err(join_error)) => assert!(join_error.is_panic(), "{join_error}"),
    Ok(Ok(result)) => assert!(result.is_err(), "the build should fail"),
  }
}
