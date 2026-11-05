use super::BuildEvent;
use crate::{types::diagnostic_options::DiagnosticOptions, types::event_kind::EventKind};

/// Why `experimental.effectServiceShaking` kept methods: a reason that turned the pass
/// off, or one that marked every object in user code as possibly holding a runtime value.
#[derive(Debug)]
pub struct EffectServiceShaking {
  pub module_id: Option<String>,
  pub tag: &'static str,
  pub reason: String,
  /// 1-based line and column of the site.
  pub position: Option<(usize, usize)>,
}

impl BuildEvent for EffectServiceShaking {
  fn kind(&self) -> EventKind {
    EventKind::EffectServiceShaking
  }

  fn message(&self, opts: &DiagnosticOptions) -> String {
    let site = match (&self.module_id, self.position) {
      (Some(id), Some((line, column))) => {
        format!(" at {line}:{column} in {}", opts.stabilize_path(id))
      }
      (Some(id), None) => format!(" in {}", opts.stabilize_path(id)),
      _ => String::new(),
    };
    format!("[{}] {}{site}", self.tag, self.reason)
  }

  fn id(&self) -> Option<String> {
    self.module_id.clone()
  }
}
