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
    // Tier banner + sound, with the event's own sound when it has one.
    let behavior = deps.notifications.behavior_for(event, deps.tiers);
    let (title, body) = notification_text(rec, event);
    if let Err(e) = deps.notifier.notify(&title, &body, &behavior).await {
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
        // The skip's own words: `draft`, the matched rule, or the size
        // against the cap.
        NotifyEvent::Skipped => match rec.skip_text() {
            Some(why) => format!("{key}: review skipped — {why}"),
            None => format!("{key}: review skipped"),
        },
    };
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, OrgConfig, PartialNotifications, PartialOverrides, RepoConfig};
    use crate::notify::{NotifyError, Tier, TierBehavior};
    use crate::state::{PrState, SkipReason};

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
            review_session_info: None,
            last_manifest: None,
            review_attempts: 0,
            diff_too_large: false,
            skip_detail: None,
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

    /// Ready and blocked are told apart by ear: approved plays `Glass` out
    /// of the box, blocked the urgent tier's `Basso`, and a repo's
    /// `notifications.sounds` entry replaces either.
    #[tokio::test]
    async fn ready_and_blocked_reviews_play_their_own_sounds() {
        let tiers = TierTable::builtin();
        let effects = [
            Effect::Notify(NotifyEvent::ReviewCompleteApproved),
            Effect::Notify(NotifyEvent::ReviewCompleteBlocking),
        ];
        let mut custom = Notifications::default();
        custom.sounds.on_review_complete_approved = Some("none".into());
        custom.sounds.on_review_complete_blocking = Some("Sosumi".into());
        for (notifications, expected) in [
            (
                Notifications::default(),
                [Some("Glass".to_string()), Some("Basso".to_string())],
            ),
            (custom, [None, Some("Sosumi".to_string())]),
        ] {
            let notifier = Recording::default();
            let deps = EffectsDeps {
                notifier: &notifier,
                tiers: &tiers,
                notifications: &notifications,
            };
            execute_poll_effects(&deps, &rec("o/r"), &effects).await;
            let sounds: Vec<Option<String>> = notifier
                .0
                .into_inner()
                .unwrap()
                .into_iter()
                .map(|b| b.sound)
                .collect();
            assert_eq!(sounds, expected);
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

    /// Notifications are best-effort: a notifier that fails does not stop
    /// the effects after it.
    #[tokio::test]
    async fn a_failing_notifier_does_not_stop_later_effects() {
        struct Failing(std::sync::Mutex<Vec<String>>);
        impl Notifier for Failing {
            async fn notify(
                &self,
                _title: &str,
                body: &str,
                _behavior: &TierBehavior,
            ) -> Result<(), NotifyError> {
                self.0.lock().unwrap().push(body.to_string());
                Err(NotifyError::Io(std::io::Error::other("no notifier")))
            }
        }
        let tiers = TierTable::builtin();
        let notifications = Notifications::default();
        let notifier = Failing(Default::default());
        let deps = EffectsDeps {
            notifier: &notifier,
            tiers: &tiers,
            notifications: &notifications,
        };
        let effects = [
            Effect::Notify(NotifyEvent::Failure),
            Effect::Notify(NotifyEvent::Merged),
        ];
        execute_poll_effects(&deps, &rec("o/r"), &effects).await;
        assert_eq!(
            notifier.0.into_inner().unwrap(),
            [
                "o/r#7: review failed \u{2014} check agent and logs",
                "o/r#7 merged"
            ]
        );
    }

    /// Spec §3.6: what each event says. An approved verdict is worded as a
    /// passed review, never as an approval; PR titles appear as text only
    /// where the event is about a new PR.
    #[test]
    fn notification_text_per_event() {
        let mut pr = rec("o/r");
        pr.title = "Fix `$(id)` \"quoted\"".into();
        let body = |pr: &PrRecord, event| {
            let (title, body) = notification_text(pr, event);
            assert_eq!(title, "PRX");
            body
        };
        assert_eq!(
            body(&pr, NotifyEvent::NewPr),
            "new PR o/r#7: Fix `$(id)` \"quoted\""
        );
        assert_eq!(
            body(&pr, NotifyEvent::ReviewStarted),
            "o/r#7: review started"
        );
        assert_eq!(
            body(&pr, NotifyEvent::ReviewCompleteApproved),
            "o/r#7 review passed: no blocking issues"
        );
        assert_eq!(body(&pr, NotifyEvent::Closed), "o/r#7 closed");
        assert_eq!(
            body(&pr, NotifyEvent::NewCommits),
            "o/r#7: new commits \u{2014} re-review pending"
        );
        assert_eq!(
            body(&pr, NotifyEvent::ReReviewTriggered),
            "o/r#7: re-review triggered"
        );
        // A blocking verdict with no blocking finding recorded (a bare
        // `changes_requested`) still reads as one issue, never "0 issues".
        assert_eq!(
            body(&pr, NotifyEvent::ReviewCompleteBlocking),
            "o/r#7 review: 1 blocking issue(s) found"
        );
        pr.blocking_count = 4;
        assert_eq!(
            body(&pr, NotifyEvent::ReviewCompleteBlocking),
            "o/r#7 review: 4 blocking issue(s) found"
        );
        assert_eq!(body(&pr, NotifyEvent::Skipped), "o/r#7: review skipped");
        pr.state = PrState::Pending;
        pr.diff_too_large = true;
        assert_eq!(
            body(&pr, NotifyEvent::Skipped),
            "o/r#7: review skipped \u{2014} diff too large"
        );
        pr.skip_detail = Some("2,140 lines changed (limit 1,500)".into());
        assert_eq!(
            body(&pr, NotifyEvent::Skipped),
            "o/r#7: review skipped \u{2014} 2,140 lines changed (limit 1,500)"
        );
        pr.diff_too_large = false;
        pr.state = PrState::Skipped;
        pr.skip_reason = Some(SkipReason::Config);
        pr.skip_detail = Some("author bot excluded".into());
        assert_eq!(
            body(&pr, NotifyEvent::Skipped),
            "o/r#7: review skipped \u{2014} author bot excluded"
        );
    }
}
