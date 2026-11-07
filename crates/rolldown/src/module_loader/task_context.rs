use std::{future::Future, panic::AssertUnwindSafe};

use futures::FutureExt;
use oxc::transformer_plugins::ReplaceGlobalDefinesConfig;
use rolldown_common::ModuleLoaderMsg;
use rolldown_error::BuildResult;
use rolldown_fs::FileSystem;
use rolldown_plugin::SharedPluginDriver;
use rolldown_std_utils::{discard_panic_payload, panic_payload_message};

use crate::{SharedOptions, SharedResolver};

/// Used to store common data shared between all tasks.
pub struct TaskContext<Fs: FileSystem> {
  pub options: SharedOptions,
  pub tx: tokio::sync::mpsc::UnboundedSender<ModuleLoaderMsg>,
  pub resolver: SharedResolver<Fs>,
  pub fs: Fs,
  pub plugin_driver: SharedPluginDriver,
  pub meta: TaskContextMeta,
}

pub struct TaskContextMeta {
  pub replace_global_define_config: Option<ReplaceGlobalDefinesConfig>,
}

/// Runs the body of a module task, turning a panic into a build error.
///
/// The loader counts the tasks it spawned and waits for one `ModuleLoaderMsg` from each, on a
/// channel whose sender it holds itself. A task that unwound would send nothing, and the build
/// would wait forever. A panic in a plugin hook, or in code the task runs such as the minifier's
/// dead-code elimination, is reported as an error for that module instead, and the scan ends
/// like any failed scan. See `internal-docs/bundler-data-lifecycle/implementation.md`.
pub async fn catch_task_panic(
  module_id: &str,
  body: impl Future<Output = BuildResult<()>>,
) -> BuildResult<()> {
  match AssertUnwindSafe(body).catch_unwind().await {
    Ok(result) => result,
    Err(payload) => {
      let message = panic_payload_message(&*payload);
      discard_panic_payload(payload);
      Err(anyhow::anyhow!("Panicked while loading module `{module_id}`: {message}"))?
    }
  }
}
