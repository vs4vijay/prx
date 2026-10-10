//! Shared executor for machine effect intents ([`Effect`]): desktop
//! notifications through the configured tier behavior and history appends.
//! Used by both `prx status` (poll-driven transitions) and the review
//! pipeline so every path side-effects identically.

use crate::config::{EffectiveRepo, Notifications};
use crate::history::{self, HistoryEntry};
use crate::machine::Effect;
use crate::notify::{Notifier, NotifyEvent, TierTable};
use crate::state::PrRecord;
use chrono::Utc;

/// Everything the executor needs beyond the effects themselves.
pub struct EffectsDeps<'a, N: Notifier> {
    pub notifier: &'a N,
    pub tiers: &'a TierTable,
    /// Event → tier mapping from the effective config (A10).
    pub notifications: &'a Notifications,
}

impl<'a, N: Notifier> EffectsDeps<'a, N> {
    /// Deps for effects on one repo's PRs: the tier mapping is that repo's
    /// effective `notifications` (defaults ← org ← repo, design D13).
    pub fn for_repo(notifier: &'a N, tiers: &'a TierTable, repo: &'a EffectiveRepo) -> Self {
        EffectsDeps {
            notifier,
            tiers,
            notifications: &repo.notifications,
        }
    }
}

/// Metadata folded into history entries; unknown for poll-driven effects.
pub struct EffectsEnrichment<'a> {
    /// Agent key configured for the repo (`defaults.agent` / repo override).
    pub agent: Option<&'a str>,
    /// Agent-reported review duration, when the effect came from a run.
    pub duration_seconds: Option<u64>,
    /// Blocking finding bodies from the reviewed manifest (A18: raw text for
    /// insights; truncated per entry by the history layer). Empty for
    /// poll-driven effects.
    pub blocking_texts: Vec<String>,
    /// Design D4: whether a completed run wrote to GitHub (`Some(false)`
    /// for monitor mode or a `post: false` run); `None` when unknown.
    pub posted: Option<bool>,
    /// Design D11: the run's reported token usage (completed and failed
    /// runs); `None` when unknown.
    pub usage: Option<crate::usage::TokenUsage>,
    /// Design B3: the brief a completed review produced in its manifest,
    /// recorded on its history entry; `None` when it produced none.
    pub brief: Option<String>,
}

impl EffectsEnrichment<'_> {
    fn none() -> EffectsEnrichment<'static> {
        EffectsEnrichment {
            agent: None,
            duration_seconds: None,
            blocking_texts: Vec::new(),
            posted: None,
            usage: None,
            brief: None,
        }
    }
}

/// Execute every effect intent. Notifications and history appends are
/// best-effort: failures are logged and never abort the caller's flow
/// (DEV-PLAN §1).
pub async fn execute_effects<N: Notifier>(
    deps: &EffectsDeps<'_, N>,
    rec: &PrRecord,
    enrichment: &EffectsEnrichment<'_>,
    effects: &[Effect],
) {
    for effect in effects {
        match effect {
            Effect::Notify(event) => notify(deps, rec, *event).await,
            Effect::HistoryAppend(event) => {
                let mut entry = HistoryEntry::from_effect(
                    event,
                    rec,
                    enrichment.agent,
                    enrichment.duration_seconds,
                    enrichment.blocking_texts.clone(),
                    Utc::now(),
                );
                match &mut entry {
                    HistoryEntry::ReviewCompleted {
                        posted,
                        usage,
                        brief,
                        ..
                    } => {
                        *posted = enrichment.posted;
                        usage.clone_from(&enrichment.usage);
                        brief.clone_from(&enrichment.brief);
                    }
                    HistoryEntry::ReviewFailed { usage, .. } => {
                        usage.clone_from(&enrichment.usage);
                    }
                    _ => {}
                }
                if let Err(e) = history::append(&crate::paths::history_file(), &entry) {
                    tracing::warn!("failed to append history for {}: {e}", rec.key());
                }
            }
        }
    }
}

/// Convenience wrapper for poll-driven effects (no agent, no duration).
pub async fn execute_poll_effects<N: Notifier>(
    deps: &EffectsDeps<'_, N>,
    rec: &PrRecord,
    effects: &[Effect],
) {
    execute_effects(deps, rec, &EffectsEnrichment::none(), effects).await;
}

async fn notify<N: Notifier>(deps: &EffectsDeps<'_, N>, rec: &PrRecord, event: NotifyEvent) {
    // Design D6: the global master switch stops delivery only — history
    // appends and state updates around this call proceed as usual.
    if !deps.notifications.enabled {
        tracing::debug!(
            "notifications disabled; not delivering {} for {}",
            event.config_key(),
            rec.key()
        );
        return;
    }
    let tier = deps.notifications.tier_for(event);
    let behavior = deps.tiers.behavior(tier);
    let (title, body) = notification_text(rec, event);
    if let Err(e) = deps.notifier.notify(&title, &body, behavior).await {
        tracing::debug!("notification for {} not delivered: {e}", rec.key());
    }
}

/// Human-facing notification copy per event (spec §3.6).
fn notification_text(rec: &PrRecord, event: NotifyEvent) -> (String, String) {
    let title = "PRX".to_string();
    let key = rec.key();
    let body = match event {
        NotifyEvent::NewPr => format!("new PR {key}: {}", rec.title),
        NotifyEvent::ReviewStarted => format!("{key}: review started"),
        NotifyEvent::ReviewCompleteApproved => format!("{key} review passed: no blocking issues"),
        NotifyEvent::ReviewCompleteBlocking => format!(
            "{key} review: {} blocking issue(s) found",
            rec.blocking_count.max(1)
        ),
        NotifyEvent::NewCommits => format!("{key}: new commits — re-review pending"),
        NotifyEvent::ReReviewTriggered => format!("{key}: re-review triggered"),
        NotifyEvent::Merged => format!("{key} merged"),
        NotifyEvent::Closed => format!("{key} closed"),
        NotifyEvent::Failure => format!("{key}: review failed — check agent and logs"),
        NotifyEvent::Skipped => match rec.diff_too_large {
            true => format!("{key}: review skipped — diff too large"),
            false => format!("{key}: review skipped"),
        },
    };
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, OrgConfig, PartialNotifications, PartialOverrides, RepoConfig};
    use crate::notify::{NotifyError, Tier, TierBehavior};
    use crate::state::PrState;

    /// Records the tier behavior each notification was delivered with.
    #[derive(Default)]
    struct Recording(std::sync::Mutex<Vec<TierBehavior>>);

    impl Notifier for Recording {
        async fn notify(
            &self,
            _title: &str,
            _body: &str,
            behavior: &TierBehavior,
        ) -> Result<(), NotifyError> {
            self.0.lock().unwrap().push(behavior.clone());
            Ok(())
        }
    }

    fn rec(repo: &str) -> PrRecord {
        PrRecord {
            repo: repo.into(),
            number: 7,
            title: "t".into(),
            author: "a".into(),
            head_branch: "b".into(),
            head_sha: "s".into(),
            is_draft: false,
            gh_created_at: Utc::now(),
            state: PrState::Merged,
            previous_state: None,
            skip_reason: None,
            snooze_until: None,
            snooze_until_push: false,
            review_anchor_at: None,
            last_reviewed_sha: None,
            last_review_at: None,
            last_verdict: None,
            last_summary: None,
            blocking_count: 0,
            nit_count: 0,
            review_session: None,
            review_attempts: 0,
            diff_too_large: false,
            human_approved_at: None,
            exec_summary: None,
            brief: None,
            updated_at: Utc::now(),
        }
    }

    fn merged_override(tier: Tier) -> PartialOverrides {
        PartialOverrides {
            notifications: Some(PartialNotifications {
                on_merged: Some(tier),
                ..PartialNotifications::default()
            }),
            ..PartialOverrides::default()
        }
    }

    /// Design D6: the master switch silences delivery for every event, at
    /// any tier, and only delivery.
    #[tokio::test]
    async fn master_switch_off_delivers_nothing() {
        let tiers = TierTable::builtin();
        let mut notifications = Notifications {
            on_merged: Tier::Urgent,
            ..Notifications::default()
        };
        let effects = [
            Effect::Notify(NotifyEvent::ReviewStarted),
            Effect::Notify(NotifyEvent::Merged),
        ];
        for (enabled, expected) in [(false, 0), (true, 2)] {
            notifications.enabled = enabled;
            let notifier = Recording::default();
            let deps = EffectsDeps {
                notifier: &notifier,
                tiers: &tiers,
                notifications: &notifications,
            };
            execute_poll_effects(&deps, &rec("o/r"), &effects).await;
            assert_eq!(notifier.0.into_inner().unwrap().len(), expected);
        }
    }

    /// Design D13: an org/repo `notifications` override picks the tier for
    /// that repo's events; other repos keep `defaults.notifications`.
    #[tokio::test]
    async fn per_repo_notification_overrides_pick_the_delivered_tier() {
        let mut config = Config::default();
        config.orgs.push(OrgConfig {
            org: "acme".into(),
            overrides: merged_override(Tier::Urgent),
        });
        for name in ["acme/web", "other/repo"] {
            config.repos.push(RepoConfig {
                name: name.into(),
                overrides: PartialOverrides::default(),
            });
        }
        config.repos.push(RepoConfig {
            name: "other/loud".into(),
            overrides: merged_override(Tier::Informational),
        });
        let tiers = TierTable::builtin();
        let mut delivered = Vec::new();
        for repo in config.effective_repos().unwrap() {
            let notifier = Recording::default();
            let deps = EffectsDeps::for_repo(&notifier, &tiers, &repo);
            let effects = [Effect::Notify(NotifyEvent::Merged)];
            execute_poll_effects(&deps, &rec(&repo.name), &effects).await;
            delivered.push((repo.name, notifier.0.into_inner().unwrap()));
        }
        let behavior = |name: &str| {
            delivered
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, b)| b[0].clone())
                .unwrap()
        };
        assert_eq!(behavior("acme/web"), tiers.urgent, "org override");
        assert_eq!(behavior("other/loud"), tiers.informational, "repo override");
        assert_eq!(
            behavior("other/repo"),
            tiers.silent,
            "defaults: merged is silent"
        );
    }
}
