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

impl NotifyEvent {
    /// Every event, in the order of the `defaults.notifications` keys.
    pub const ALL: [NotifyEvent; 10] = [
        NotifyEvent::NewPr,
        NotifyEvent::ReviewStarted,
        NotifyEvent::ReviewCompleteApproved,
        NotifyEvent::ReviewCompleteBlocking,
        NotifyEvent::NewCommits,
        NotifyEvent::ReReviewTriggered,
        NotifyEvent::Merged,
        NotifyEvent::Closed,
        NotifyEvent::Failure,
        NotifyEvent::Skipped,
    ];

    /// The sound this event plays when `notifications.sounds` does not name
    /// one: the ready event (`approved` verdict) has its own, so it is told
    /// apart from a blocked review by ear. Every other event plays its
    /// tier's sound.
    pub fn builtin_sound(&self) -> Option<&'static str> {
        match self {
            NotifyEvent::ReviewCompleteApproved => Some("Glass"),
            _ => None,
        }
    }
}

/// The `notifications.sounds` value that means "no sound for this event",
/// whatever its tier plays (case does not matter).
pub const NO_SOUND: &str = "none";

/// Check one sound name from the config: a macOS sound name (`Glass`,
/// `Basso`, a custom sound in `~/Library/Sounds`) or [`NO_SOUND`]. The name
/// reaches the notifier as an argument, never as script text; this only
/// refuses values that cannot be a sound name, so a typo surfaces in
/// validation instead of as a silent banner.
pub fn validate_sound_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!(
            "sound must not be empty: name a sound such as Glass, or use `{NO_SOUND}`"
        ));
    }
    if name.len() > 64 {
        return Err("sound name is longer than 64 characters".to_string());
    }
    if name.contains('/') || name.chars().any(char::is_control) {
        return Err(format!(
            "'{}' is not a sound name: give the name only (e.g. Glass), not a path",
            name.escape_debug()
        ));
    }
    Ok(())
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

/// The command that raises one banner on this platform, or `None` when the
/// tier has no banner.
///
/// The notification text reaches the command as arguments of its own, never
/// as part of a script: a body can hold a PR title, which is untrusted, and
/// text spliced into AppleScript source would be run as AppleScript. On
/// macOS the script is a fixed `on run argv` handler given with `-e` (a bare
/// argument is read as a script *file*), and it reads title, body, and the
/// tier's sound from `argv`.
fn banner_command(
    title: &str,
    body: &str,
    behavior: &TierBehavior,
) -> Option<(&'static str, Vec<String>)> {
    if !behavior.banner {
        return None;
    }
    if !cfg!(target_os = "macos") {
        return Some(("notify-send", vec![title.to_string(), body.to_string()]));
    }
    let sound = behavior.sound.as_deref().filter(|s| !s.trim().is_empty());
    let display = match sound {
        Some(_) => {
            "display notification (item 2 of argv) with title (item 1 of argv) \
             sound name (item 3 of argv)"
        }
        None => "display notification (item 2 of argv) with title (item 1 of argv)",
    };
    let mut args: Vec<String> = ["-e", "on run argv", "-e", display, "-e", "end run", "--"]
        .map(String::from)
        .to_vec();
    args.push(title.to_string());
    args.push(body.to_string());
    args.extend(sound.map(String::from));
    Some(("osascript", args))
}

impl Notifier for CommandNotifier {
    async fn notify(
        &self,
        title: &str,
        body: &str,
        behavior: &TierBehavior,
    ) -> Result<(), NotifyError> {
        let Some((program, args)) = banner_command(title, body, behavior) else {
            return Ok(());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Notifications;

    const EVERY_EVENT: [NotifyEvent; 10] = NotifyEvent::ALL;

    fn banner(sound: Option<&str>) -> TierBehavior {
        TierBehavior {
            banner: true,
            sound: sound.map(String::from),
        }
    }

    /// The documented defaults: urgent = banner + `Basso`, informational =
    /// banner without a sound, silent = nothing.
    #[test]
    fn builtin_tiers_match_the_documented_behavior() {
        let table = TierTable::default();
        assert_eq!(table, TierTable::builtin());
        assert_eq!(table.behavior(Tier::Urgent), &banner(Some("Basso")));
        assert_eq!(table.behavior(Tier::Informational), &banner(None));
        assert_eq!(
            table.behavior(Tier::Silent),
            &TierBehavior {
                banner: false,
                sound: None
            }
        );
    }

    /// `behavior` returns the row of the tier asked for, also in a table
    /// where no two rows agree.
    #[test]
    fn behavior_looks_up_the_row_of_its_tier() {
        let table = TierTable {
            urgent: banner(Some("Glass")),
            informational: TierBehavior {
                banner: false,
                sound: Some("Ping".into()),
            },
            silent: banner(None),
        };
        assert_eq!(table.behavior(Tier::Urgent).sound.as_deref(), Some("Glass"));
        assert_eq!(
            table.behavior(Tier::Informational).sound.as_deref(),
            Some("Ping")
        );
        assert!(!table.behavior(Tier::Informational).banner);
        assert_eq!(table.behavior(Tier::Silent), &banner(None));
    }

    /// Tier names on the wire (config and IPC) are lower case; anything
    /// else is rejected rather than read as some tier.
    #[test]
    fn tier_names_are_lower_case_on_the_wire() {
        for (tier, name) in [
            (Tier::Urgent, "urgent"),
            (Tier::Informational, "informational"),
            (Tier::Silent, "silent"),
        ] {
            assert_eq!(serde_json::to_value(tier).unwrap(), name);
            assert_eq!(serde_json::from_value::<Tier>(name.into()).unwrap(), tier);
        }
        for bad in ["Urgent", "info", "", "loud"] {
            assert!(
                serde_json::from_value::<Tier>(bad.into()).is_err(),
                "{bad:?} must not parse as a tier"
            );
        }
    }

    /// The `notification_tiers` section keeps its shape: three tier keys,
    /// each `{banner, sound}`, with `sound: null` meaning no sound.
    #[test]
    fn tier_table_round_trips_in_the_config_shape() {
        let yaml = "urgent: {banner: true, sound: Glass}\n\
                    informational: {banner: false, sound: null}\n\
                    silent: {banner: true, sound: null}\n";
        let table: TierTable = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(table.urgent, banner(Some("Glass")));
        assert!(!table.informational.banner);
        assert_eq!(table.silent, banner(None));

        let json = serde_json::to_value(&table).unwrap();
        assert_eq!(json["urgent"]["sound"], "Glass");
        assert!(json["informational"]["sound"].is_null());
        assert_eq!(serde_json::from_value::<TierTable>(json).unwrap(), table);
    }

    /// Every event has its own `defaults.notifications` key, and the key
    /// names the setting that decides the event's tier: retiering one key
    /// moves exactly the one event it belongs to.
    #[test]
    fn each_event_is_tiered_by_its_own_config_key() {
        let defaults = Notifications::default();
        let keys: Vec<&str> = EVERY_EVENT.iter().map(NotifyEvent::config_key).collect();
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), EVERY_EVENT.len(), "duplicate key in {keys:?}");

        // The config section has exactly these keys, plus the master switch.
        let section = serde_json::to_value(&defaults).unwrap();
        let mut in_config: Vec<&str> = section
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|key| !["enabled", "menubar_bell", "sounds"].contains(key))
            .collect();
        in_config.sort_unstable();
        assert_eq!(in_config, unique.into_iter().collect::<Vec<_>>());

        for event in EVERY_EVENT {
            // Flip this event's key to a tier it does not have by default.
            let other = match defaults.tier_for(event) {
                Tier::Urgent => Tier::Silent,
                _ => Tier::Urgent,
            };
            let mut edited = section.clone();
            edited[event.config_key()] = serde_json::to_value(other).unwrap();
            let edited: Notifications = serde_json::from_value(edited).unwrap();
            for probe in EVERY_EVENT {
                let expected = if probe == event {
                    other
                } else {
                    defaults.tier_for(probe)
                };
                assert_eq!(
                    edited.tier_for(probe),
                    expected,
                    "{} changed; checking {}",
                    event.config_key(),
                    probe.config_key()
                );
            }
        }
    }

    /// The default event → tier mapping, as documented: blocking reviews
    /// and failures are urgent; merges, closes, and skips are silent; the
    /// rest is informational.
    #[test]
    fn default_event_tiers_match_the_documented_mapping() {
        let defaults = Notifications::default();
        for event in EVERY_EVENT {
            let expected = match event {
                NotifyEvent::ReviewCompleteBlocking | NotifyEvent::Failure => Tier::Urgent,
                NotifyEvent::Merged | NotifyEvent::Closed | NotifyEvent::Skipped => Tier::Silent,
                NotifyEvent::NewPr
                | NotifyEvent::ReviewStarted
                | NotifyEvent::ReviewCompleteApproved
                | NotifyEvent::NewCommits
                | NotifyEvent::ReReviewTriggered => Tier::Informational,
            };
            assert_eq!(defaults.tier_for(event), expected, "{}", event.config_key());
        }
    }

    /// A tier without a banner raises nothing: there is no command to run,
    /// whatever its sound setting says.
    #[test]
    fn a_tier_without_a_banner_has_no_command() {
        let silent = TierBehavior {
            banner: false,
            sound: Some("Basso".into()),
        };
        assert_eq!(banner_command("PRX", "org/one#1 merged", &silent), None);
    }

    /// PR titles are untrusted (invariant 3) and end up in banner bodies.
    /// Title and body must reach the notification command as arguments of
    /// their own, byte for byte, and never inside the script it runs.
    #[test]
    fn notification_text_is_passed_as_arguments_never_as_script() {
        let title = "PRX";
        let body = "new PR org/one#1: x\" & (do shell script \"touch /tmp/owned\") & \"\\";
        let (program, args) = banner_command(title, body, &banner(None)).unwrap();

        let text_at = args.iter().position(|a| a == title).unwrap();
        assert_eq!(&args[text_at..], [title, body]);
        for script_part in &args[..text_at] {
            assert!(
                !script_part.contains("do shell script") && !script_part.contains("owned"),
                "notification text leaked into the script: {script_part:?}"
            );
        }
        if cfg!(target_os = "macos") {
            assert_eq!(program, "osascript");
            // Every statement is given with `-e`: osascript reads a bare
            // argument as the path of a script file and delivers nothing.
            assert_eq!(args[0], "-e");
            assert_eq!(args[text_at - 1], "--");
            let statements: Vec<&String> = args[..text_at - 1]
                .chunks(2)
                .map(|pair| {
                    assert_eq!(pair[0], "-e");
                    &pair[1]
                })
                .collect();
            assert_eq!(statements.len(), 3);
            assert!(statements[1].starts_with("display notification"));
        } else {
            assert_eq!(program, "notify-send");
            assert_eq!(text_at, 0);
        }
    }

    /// The tier's sound is played with the banner on macOS — passed as an
    /// argument like the text — and a blank sound name is no sound.
    #[cfg(target_os = "macos")]
    #[test]
    fn tier_sound_is_passed_to_the_banner_on_macos() {
        let (_, with_sound) = banner_command("PRX", "body", &banner(Some("Basso"))).unwrap();
        assert_eq!(with_sound.last().map(String::as_str), Some("Basso"));
        assert!(with_sound.iter().any(|a| a.contains("sound name")));

        for quiet in [None, Some(""), Some("  ")] {
            let (_, args) = banner_command("PRX", "body", &banner(quiet)).unwrap();
            assert_eq!(args.last().map(String::as_str), Some("body"));
            assert!(!args.iter().any(|a| a.contains("sound name")));
        }
    }

    /// The script handed to the real `osascript` compiles and receives the
    /// text as `argv` unchanged. It is run with `display notification`
    /// replaced by a statement returning what it was given, so nothing is
    /// shown on the desktop.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_osascript_receives_hostile_text_as_plain_arguments() {
        let body = "x\" & (do shell script \"exit 9\") & \"\\ -e 'q'";
        let (program, mut args) = banner_command("-PRX", body, &banner(None)).unwrap();
        let display = args
            .iter_mut()
            .find(|a| a.starts_with("display notification"))
            .unwrap();
        *display = "return (item 1 of argv) & linefeed & (item 2 of argv)".to_string();

        let output = std::process::Command::new(program)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            format!("-PRX\n{body}")
        );
    }
}
