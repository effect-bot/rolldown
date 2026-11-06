use crate::types::diagnostic_options::DiagnosticOptions;

use super::BuildEvent;

/// `experimental.effectServiceShaking` could not analyse the build and kept every service
/// method. See `internal-docs/effect-service-shaking/design.md`.
#[derive(Debug)]
pub struct EffectServiceShaking {
  pub reason: String,
}

impl BuildEvent for EffectServiceShaking {
  fn kind(&self) -> crate::types::event_kind::EventKind {
    crate::types::event_kind::EventKind::EffectServiceShaking
  }

  fn message(&self, _opts: &DiagnosticOptions) -> String {
    format!("Effect service shaking kept every service method: {}.", self.reason)
  }
}
