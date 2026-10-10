//! Daemon-wide review pause (design D5): the single enqueue gate shared by
//! every automatic origin, plus the `--hours` / `--until` argument forms
//! used by `prx pause` and `POST /pause`.
//!
//! The pause itself lives in `StateFile.paused_until` (additive, schema v1),
//! so it survives daemon restarts and expires by time.

use anyhow::{bail, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

use crate::config::ReviewMode;
use crate::state::StateFile;

/// Where a review enqueue comes from. Every origin except [`SinglePr`]
/// is automatic and stops while reviews are paused.
///
/// [`SinglePr`]: EnqueueOrigin::SinglePr
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOrigin {
    /// A cron `schedule` tick's due-scan (D11).
    ScheduledTick,
    /// The interval due-scan of an unscheduled repo.
    DueScan,
    /// `prx review --all` / `POST /review {key: "all"}`.
    ReviewAll,
    /// `prx review-now` / `POST /review-now` (poll + due-scan).
    ReviewNow,
    /// An explicit `prx review org/repo#N`: bypasses the pause (the CLI
    /// notes it), never the `off` mode.
    SinglePr,
}

impl EnqueueOrigin {
    pub const ALL: [EnqueueOrigin; 5] = [
        EnqueueOrigin::ScheduledTick,
        EnqueueOrigin::DueScan,
        EnqueueOrigin::ReviewAll,
        EnqueueOrigin::ReviewNow,
        EnqueueOrigin::SinglePr,
    ];

    /// Whether this origin is automatic (subject to the pause).
    pub fn is_automatic(self) -> bool {
        self != EnqueueOrigin::SinglePr
    }
}

/// D5: the one enqueue gate — `!paused(state, now) && review_mode != off`,
/// where explicit single-PR jobs skip the pause half.
pub fn enqueue_allowed(
    state: &StateFile,
    review_mode: ReviewMode,
    origin: EnqueueOrigin,
    now: DateTime<Utc>,
) -> bool {
    if review_mode == ReviewMode::Off {
        return false;
    }
    !(origin.is_automatic() && state.is_paused(now))
}

/// Resolve the `--hours N` / `--until <time>` pair into the pause end.
/// Exactly one must be given; the result must lie in the future.
pub fn resolve_pause_until(
    hours: Option<f64>,
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let end = match (hours, until) {
        (Some(_), Some(_)) => bail!("specify either --hours or --until, not both"),
        (None, None) => bail!("specify --hours N or --until <time>"),
        (Some(h), None) => {
            if !h.is_finite() || h <= 0.0 {
                bail!("--hours must be > 0");
            }
            // Cap at ~10 years so the arithmetic cannot overflow.
            if h > 87_600.0 {
                bail!("--hours is too large (max 87600)");
            }
            now + chrono::Duration::milliseconds((h * 3_600_000.0) as i64)
        }
        (None, Some(t)) => t,
    };
    if end <= now {
        bail!(
            "the pause end {} is not in the future",
            end.format("%Y-%m-%d %H:%M UTC")
        );
    }
    Ok(end)
}

/// Parse a `--until` value: RFC 3339 (`2026-10-07T18:00:00Z`), a local
/// `YYYY-MM-DD HH:MM` (or with `T`), or a local `HH:MM` — the next such
/// wall-clock time (today, or tomorrow when it has passed).
pub fn parse_until(input: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    parse_until_in(input, now, &Local)
}

fn parse_until_in<Tz: TimeZone>(input: &str, now: DateTime<Utc>, tz: &Tz) -> Result<DateTime<Utc>> {
    let s = input.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return local_to_utc(tz, naive, input);
        }
    }
    if let Ok(time) = NaiveTime::parse_from_str(s, "%H:%M") {
        let today: NaiveDate = now.with_timezone(tz).date_naive();
        let candidate = local_to_utc(tz, today.and_time(time), input)?;
        if candidate > now {
            return Ok(candidate);
        }
        let tomorrow = today.succ_opt().unwrap_or(today);
        return local_to_utc(tz, tomorrow.and_time(time), input);
    }
    bail!(
        "cannot parse --until '{input}': use RFC 3339 (2026-10-07T18:00:00Z), \
         'YYYY-MM-DD HH:MM', or 'HH:MM' (local time)"
    )
}

fn local_to_utc<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime, input: &str) -> Result<DateTime<Utc>> {
    match tz.from_local_datetime(&naive).earliest() {
        Some(t) => Ok(t.with_timezone(&Utc)),
        None => bail!("--until '{input}' does not exist in local time (DST gap)"),
    }
}

/// Human remaining time, e.g. `1h 12m` / `5m` / `<1m`.
pub fn format_remaining(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (until - now).num_minutes();
    if mins < 1 {
        "<1m".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 48 * 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{}d {}h", mins / (24 * 60), (mins / 60) % 24)
    }
}

/// One-line pause description, e.g.
/// `reviews paused until 2026-10-07 18:00 UTC (1h 12m left)`.
pub fn describe(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!(
        "reviews paused until {} ({} left)",
        until.format("%Y-%m-%d %H:%M UTC"),
        format_remaining(until, now)
    )
}

/// The review-control header `prx status` prints above the table: the
/// pause and mode `off` on separate lines (they are different gates —
/// pause keeps polling, `off` stops it). Empty when neither applies.
pub fn status_lines(
    paused_until: Option<DateTime<Utc>>,
    review_mode: ReviewMode,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(until) = paused_until.filter(|u| now < *u) {
        lines.push(format!(
            "{} — polling continues; resume with `prx resume`",
            describe(until, now)
        ));
    }
    if review_mode == ReviewMode::Off {
        lines.push("review mode: off — polling and reviews stopped".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn state(paused_until: Option<&str>) -> StateFile {
        StateFile {
            paused_until: paused_until.map(at),
            ..StateFile::default()
        }
    }

    const NOW: &str = "2026-10-07T12:00:00Z";
    const MODES: [ReviewMode; 3] = [
        ReviewMode::Off,
        ReviewMode::Monitor,
        ReviewMode::ReviewComment,
    ];

    /// The full gate matrix: {not paused, paused, pause expired} × each
    /// review_mode × each origin.
    #[test]
    fn enqueue_gate_matrix() {
        let now = at(NOW);
        let cases = [
            ("never paused", state(None), false),
            ("paused", state(Some("2026-10-07T13:00:00Z")), true),
            ("expired", state(Some("2026-10-07T11:00:00Z")), false),
            ("ends exactly now", state(Some(NOW)), false),
        ];
        for (label, st, paused) in &cases {
            assert_eq!(st.is_paused(now), *paused, "{label}");
            for mode in MODES {
                for origin in EnqueueOrigin::ALL {
                    let expected =
                        mode != ReviewMode::Off && (!paused || origin == EnqueueOrigin::SinglePr);
                    assert_eq!(
                        enqueue_allowed(st, mode, origin, now),
                        expected,
                        "{label} × {mode:?} × {origin:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn pause_expires_by_time() {
        let st = state(Some("2026-10-07T13:00:00Z"));
        let before = at("2026-10-07T12:59:59Z");
        let after = at("2026-10-07T13:00:01Z");
        assert!(!enqueue_allowed(
            &st,
            ReviewMode::ReviewComment,
            EnqueueOrigin::DueScan,
            before
        ));
        assert!(enqueue_allowed(
            &st,
            ReviewMode::ReviewComment,
            EnqueueOrigin::DueScan,
            after
        ));
        assert_eq!(st.active_pause(after), None);
    }

    #[test]
    fn resolve_requires_exactly_one_form() {
        let now = at(NOW);
        let err = resolve_pause_until(None, None, now).unwrap_err();
        assert!(err.to_string().contains("--hours N or --until"), "{err}");
        let err =
            resolve_pause_until(Some(1.0), Some(at("2026-10-08T00:00:00Z")), now).unwrap_err();
        assert!(err.to_string().contains("not both"), "{err}");
        assert!(resolve_pause_until(Some(0.0), None, now).is_err());
        assert!(resolve_pause_until(Some(-2.0), None, now).is_err());
        assert!(resolve_pause_until(Some(f64::NAN), None, now).is_err());
        assert!(resolve_pause_until(None, Some(at("2026-10-07T11:00:00Z")), now).is_err());
        assert_eq!(
            resolve_pause_until(Some(1.5), None, now).unwrap(),
            at("2026-10-07T13:30:00Z")
        );
        assert_eq!(
            resolve_pause_until(None, Some(at("2026-10-08T09:00:00Z")), now).unwrap(),
            at("2026-10-08T09:00:00Z")
        );
    }

    #[test]
    fn parse_until_forms() {
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let now = at(NOW); // 14:00 local
        assert_eq!(
            parse_until_in("2026-10-07T18:00:00Z", now, &tz).unwrap(),
            at("2026-10-07T18:00:00Z")
        );
        assert_eq!(
            parse_until_in("2026-10-08 09:30", now, &tz).unwrap(),
            at("2026-10-08T07:30:00Z")
        );
        assert_eq!(
            parse_until_in("2026-10-08T09:30", now, &tz).unwrap(),
            at("2026-10-08T07:30:00Z")
        );
        // Later today…
        assert_eq!(
            parse_until_in("18:00", now, &tz).unwrap(),
            at("2026-10-07T16:00:00Z")
        );
        // …or tomorrow once the time has passed.
        assert_eq!(
            parse_until_in("09:00", now, &tz).unwrap(),
            at("2026-10-08T07:00:00Z")
        );
        assert!(parse_until_in("tomorrow-ish", now, &tz).is_err());
    }

    #[test]
    fn status_lines_show_pause_and_off_separately() {
        let now = at(NOW);
        let until = at("2026-10-07T13:12:00Z");
        assert!(status_lines(None, ReviewMode::ReviewComment, now).is_empty());
        assert!(
            status_lines(Some(at("2026-10-07T11:00:00Z")), ReviewMode::Monitor, now).is_empty()
        );
        let paused = status_lines(Some(until), ReviewMode::ReviewComment, now);
        assert_eq!(paused.len(), 1);
        assert!(paused[0].contains("paused until 2026-10-07 13:12 UTC (1h 12m left)"));
        let off = status_lines(None, ReviewMode::Off, now);
        assert_eq!(off, vec!["review mode: off — polling and reviews stopped"]);
        assert_eq!(status_lines(Some(until), ReviewMode::Off, now).len(), 2);
    }

    #[test]
    fn remaining_formats() {
        let now = at(NOW);
        assert_eq!(format_remaining(at("2026-10-07T12:00:30Z"), now), "<1m");
        assert_eq!(format_remaining(at("2026-10-07T12:05:00Z"), now), "5m");
        assert_eq!(format_remaining(at("2026-10-07T13:12:00Z"), now), "1h 12m");
        assert_eq!(format_remaining(at("2026-10-10T15:00:00Z"), now), "3d 3h");
    }
}
