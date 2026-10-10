//! Notification types: events, tiers, and tier behavior (DEV-PLAN §4).
//! Phase 3 wires a `Notifier` implementation into the daemon; Phase 1 only
//! consumes the event → tier mapping.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Notification urgency tiers (spec §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Urgent,
    Informational,
    Silent,
}

/// How a tier is delivered: banner and/or sound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierBehavior {
    pub banner: bool,
    pub sound: Option<String>,
}

/// The `notification_tiers` config section (audit A10: the keys ARE the event list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierTable {
    pub urgent: TierBehavior,
    pub informational: TierBehavior,
    pub silent: TierBehavior,
}

impl Default for TierTable {
    fn default() -> Self {
        TierTable::builtin()
    }
}

impl TierTable {
    /// Built-in tier behavior per spec §3.6 / DEV-PLAN §4.
    pub fn builtin() -> Self {
        TierTable {
            urgent: TierBehavior {
                banner: true,
                sound: Some("Basso".to_string()),
            },
            informational: TierBehavior {
                banner: true,
                sound: None,
            },
            silent: TierBehavior {
                banner: false,
                sound: None,
            },
        }
    }

    pub fn behavior(&self, tier: Tier) -> &TierBehavior {
        match tier {
            Tier::Urgent => &self.urgent,
            Tier::Informational => &self.informational,
            Tier::Silent => &self.silent,
        }
    }
}

/// Every state transition that can produce a user notification.
/// `config_key` is the corresponding key in `defaults.notifications`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyEvent {
    NewPr,
    /// A review began executing (design D6), scheduled or manual.
    ReviewStarted,
    ReviewCompleteApproved,
    ReviewCompleteBlocking,
    NewCommits,
    ReReviewTriggered,
    Merged,
    Closed,
    Failure,
    Skipped,
}

impl NotifyEvent {
    pub fn config_key(&self) -> &'static str {
        match self {
            NotifyEvent::NewPr => "on_new_pr",
            NotifyEvent::ReviewStarted => "on_review_started",
            NotifyEvent::ReviewCompleteApproved => "on_review_complete_approved",
            NotifyEvent::ReviewCompleteBlocking => "on_review_complete_blocking",
            NotifyEvent::NewCommits => "on_new_commits",
            NotifyEvent::ReReviewTriggered => "on_re_review_triggered",
            NotifyEvent::Merged => "on_merged",
            NotifyEvent::Closed => "on_closed",
            NotifyEvent::Failure => "on_failure",
            NotifyEvent::Skipped => "on_skipped",
        }
    }
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("notification command failed: {0}")]
    Command(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Delivery of one notification. Implementations must be best-effort and
/// never fatal (DEV-PLAN §1). Futures are `+ Send` for daemon use.
pub trait Notifier: Send + Sync {
    fn notify(
        &self,
        title: &str,
        body: &str,
        behavior: &TierBehavior,
    ) -> impl std::future::Future<Output = Result<(), NotifyError>> + Send;
}

/// Shells out to the platform notification binary; silent no-op when the
/// binary is missing or the tier has no banner.
pub struct CommandNotifier;

impl Notifier for CommandNotifier {
    async fn notify(
        &self,
        title: &str,
        body: &str,
        behavior: &TierBehavior,
    ) -> Result<(), NotifyError> {
        if !behavior.banner {
            return Ok(());
        }
        let (program, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
            (
                "osascript",
                vec![format!(
                    "display notification \"{}\" with title \"{}\"",
                    body, title
                )],
            )
        } else {
            ("notify-send", vec![title.to_string(), body.to_string()])
        };
        // Best-effort: a missing binary is fine (silent no-op).
        if crate::paths::find_in_path(program).is_none() {
            return Ok(());
        }
        let output = tokio::process::Command::new(program)
            .args(&args)
            .output()
            .await?;
        if !output.status.success() {
            return Err(NotifyError::Command(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }
}
