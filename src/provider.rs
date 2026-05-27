use crate::runner::Runner;
use crate::types::{ProviderId, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    Available,
    Unavailable(String), // human reason
}

/// One read of a provider's `status`, captured once per tick. Both
/// availability ("can it be activated") and active-ness ("is it up now") derive
/// from this single command so we don't double-spawn the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStatus {
    pub availability: Availability,
    pub active: bool,
}

/// A network provider vpnmux can drive. All methods idempotent.
pub trait Provider {
    fn id(&self) -> ProviderId;
    /// Read-only single probe: availability + active-ness from one command.
    fn status(&self, r: &dyn Runner) -> ProviderStatus;
    fn activate(&self, r: &dyn Runner) -> Result<()>;
    fn deactivate(&self, r: &dyn Runner) -> Result<()>;

    /// Read-only: can it be activated right now? (installed, daemon up, authed)
    fn check(&self, r: &dyn Runner) -> Availability {
        self.status(r).availability
    }
    /// Is it currently connected/up?
    fn is_active(&self, r: &dyn Runner) -> bool {
        self.status(r).active
    }
}
