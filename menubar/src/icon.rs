//! Status-bar template icons (design V2, task 7.3; menu bar redesign in
//! design-audit Part 0). The geometry mirrors the SVG master
//! `assets/menubar-glyph.svg` (the "Verdict X": a check crossed by an
//! interrupted counter-stroke) in an 18x18-point space and is rasterized at
//! 1x/2x/3x (18/36/54 px). The shell hands all three to AppKit as
//! representations of ONE template `NSImage` ([`nsimage`]): black ink with
//! coverage in alpha, so macOS tints it for light, dark, and tinted menu bars
//! and retina bars draw the matching scale instead of upscaling one raster.
//! `svg_master_matches_the_geometry` pins the constants below to the master.
//!
//! States differ by shape, never color. Idle is the full mark; every other
//! state drops the counter-stroke's lower segment and draws one badge in the
//! bottom-right corner, with the mark knocked out around it:
//!
//! - [`IconState::Idle`]: the Verdict X.
//! - [`IconState::Attention`]: a solid dot (the notification-dot idiom).
//! - [`IconState::Reviewing`]: three small dots (work in progress).
//! - [`IconState::Paused`]: pause bars (a pause, or review mode `off`).
//! - [`IconState::Offline`]: the mark dimmed, plus a slashed ring.
//!
//! The attention COUNT is not drawn into the glyph: it is the status item's
//! title ([`count_title`]), set in the menu bar's own font right of the
//! glyph — the way Mail-style counters and most PR menu bar apps show it.
//! A 3x5-pixel digit in a 7 pt badge was unreadable at 1x and crowded the
//! mark; real text is crisp at every scale, tints with the bar, and is read
//! by VoiceOver. The dot keeps the state visible on its own (the spec's
//! glyph-only distinctness) when the title is hidden or truncated.

use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapFormat,
    NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage,
};
use objc2_foundation::{NSArray, NSSize};

/// Icon size in points (the menu bar item's logical size).
pub const SIZE: u32 = 18;
/// Backing scales rendered into the one `NSImage` (18/36/54 px).
pub const SCALES: [u32; 3] = [1, 2, 3];
const SUBSAMPLES: u32 = 4;

type Pt = (f64, f64);

/// The Verdict X, mapped from its 24-grid master by `p' = 9 + (p - 12) * 0.8`.
/// The check (approve): short arm, vertex, long arm. Round caps and joins.
const CHECK: [Pt; 3] = [(3.08, 10.12), (6.52, 13.56), (15.08, 3.08)];
const CHECK_HALF_W: f64 = 0.85;
/// The counter-stroke (request changes), interrupted where it would cross
/// the check: the gap is the human's call.
const COUNTER_UPPER: [Pt; 2] = [(3.24, 3.24), (7.56, 7.56)];
const COUNTER_LOWER: [Pt; 2] = [(11.56, 11.56), (14.92, 14.92)];
const COUNTER_HALF_W: f64 = 0.75;

/// Badge corner (bottom-right): its center and the radius knocked out of
/// the mark around it (badges stay within r = 4 pt; 1 pt clearance).
const BADGE_CENTER: Pt = (14.0, 14.0);
const BADGE_CLEAR_R: f64 = 5.0;
/// Attention: one solid dot.
const ATTENTION_R: f64 = 3.25;
/// Reviewing: three dots.
const DOTS: [Pt; 3] = [(11.2, 14.0), (14.0, 14.0), (16.8, 14.0)];
const DOT_R: f64 = 1.1;
/// Paused: two pause bars `(x, y, w, h)`, corner radius 0.5.
const BARS: [(f64, f64, f64, f64); 2] = [(11.0, 11.0, 2.0, 6.0), (15.0, 11.0, 2.0, 6.0)];
const BAR_RADIUS: f64 = 0.5;
/// Offline: slashed ring, and the mark dimmed to this alpha.
const RING_R: f64 = 3.25;
const SLASH: [Pt; 2] = [(11.7, 11.7), (16.3, 16.3)];
const OFFLINE_HALF_W: f64 = 0.625;
const OFFLINE_MARK_ALPHA: f64 = 0.45;

/// Largest count the title spells out; above it the title reads "99+".
pub const MAX_TITLE_COUNT: usize = 99;

/// The glyph states the shell can present. Counts never change the glyph
/// (the count is the status item's title), so a state change is the only
/// reason to re-render the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    /// Nothing needs attention: the Verdict X, no badge.
    Idle,
    /// PRs need attention (changes requested + in review).
    Attention,
    /// Reviews in flight (`StatusResponse.in_flight`).
    Reviewing,
    /// No automatic reviews right now: a daemon-wide pause, or review mode
    /// `off`. The menu header says which.
    Paused,
    /// Daemon unreachable: dimmed mark with a slashed ring.
    Offline,
}

impl IconState {
    /// The state for a menu model. Precedence: offline, then paused /
    /// stopped (nothing is reviewed automatically — the state the user
    /// must not forget), then reviews in flight (so a running review is
    /// visible even with attention pending; the count stays in the title),
    /// then attention, else idle.
    pub fn for_model(model: &crate::model::MenuModel) -> IconState {
        if model.offline {
            IconState::Offline
        } else if model.stopped || model.paused {
            IconState::Paused
        } else if model.in_flight > 0 {
            IconState::Reviewing
        } else if model.attention > 0 {
            IconState::Attention
        } else {
            IconState::Idle
        }
    }
}

/// The status item's title: the attention count beside the glyph, in the
/// menu bar font (`None` hides it). Shown whenever the daemon answers and
/// something needs attention, whatever the glyph state — a running review
/// or a pause does not make the waiting PRs go away. Offline shows none:
/// there is no fresh count to show.
pub fn count_title(model: &crate::model::MenuModel) -> Option<String> {
    if model.offline || model.attention == 0 {
        return None;
    }
    Some(if model.attention > MAX_TITLE_COUNT {
        format!("{MAX_TITLE_COUNT}+")
    } else {
        model.attention.to_string()
    })
}

/// The status item's tooltip (also its accessibility help): the state in
/// words, since the glyph and a bare number say nothing to VoiceOver.
pub fn tooltip(model: &crate::model::MenuModel) -> String {
    let mut parts = vec!["PRX".to_string()];
    if model.offline {
        parts.push("daemon offline".into());
    } else {
        parts.push(match model.attention {
            0 => "all clear".into(),
            1 => "1 PR needs attention".into(),
            n => format!("{n} PRs need attention"),
        });
        if model.stopped {
            parts.push("reviews off".into());
        } else if model.paused {
            parts.push("reviews paused".into());
        } else if model.in_flight > 0 {
            parts.push("reviewing".into());
        }
    }
    parts.join(" \u{b7} ")
}

/// The menu bar's effective appearance. Template images are tinted by
/// AppKit, so the pixels no longer depend on it; the shell still samples it
/// to re-apply the image on a light/dark switch, which is harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// A 1x `tray_icon::Icon` (18 px) used only to create the status item with
/// the right point size; [`apply`] then swaps in the multi-scale template.
pub fn icon(state: IconState, _appearance: Appearance) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(rgba(state, 1), SIZE, SIZE).expect("icon geometry is valid")
}

/// Straight (non-premultiplied) RGBA for `state` at `scale` (pixel side
/// `SIZE * scale`): black ink, coverage in alpha (template-image pixels).
pub fn rgba(state: IconState, scale: u32) -> Vec<u8> {
    let side = SIZE * scale;
    let step = f64::from(SUBSAMPLES * scale);
    let samples = f64::from(SUBSAMPLES * SUBSAMPLES);
    let mut out = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let mut coverage = 0.0f64;
            for sub_y in 0..SUBSAMPLES {
                for sub_x in 0..SUBSAMPLES {
                    // Sample point in the 18-point geometry space.
                    let px = (f64::from(x * SUBSAMPLES + sub_x) + 0.5) / step;
                    let py = (f64::from(y * SUBSAMPLES + sub_y) + 0.5) / step;
                    coverage += sample(px, py, state);
                }
            }
            let alpha = (coverage / samples * 255.0).round() as u8;
            out.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    out
}

/// Ink alpha (0..=1) at one sample point.
fn sample(x: f64, y: f64, state: IconState) -> f64 {
    let on = |hit: bool| if hit { 1.0 } else { 0.0 };
    let p = (x, y);
    let cut_mark = || mark(x, y, false) && dist(p, BADGE_CENTER) > BADGE_CLEAR_R;
    match state {
        IconState::Idle => on(mark(x, y, true)),
        IconState::Attention => on(dist(p, BADGE_CENTER) <= ATTENTION_R || cut_mark()),
        IconState::Reviewing => on(DOTS.iter().any(|&c| dist(p, c) <= DOT_R) || cut_mark()),
        IconState::Paused => on(BARS
            .iter()
            .any(|&(bx, by, w, h)| in_round_rect(x, y, (bx, by, w, h), BAR_RADIUS))
            || cut_mark()),
        IconState::Offline => {
            let ring = (dist(p, BADGE_CENTER) - RING_R).abs() <= OFFLINE_HALF_W;
            if ring || seg_dist(p, SLASH[0], SLASH[1]) <= OFFLINE_HALF_W {
                1.0
            } else if cut_mark() {
                OFFLINE_MARK_ALPHA
            } else {
                0.0
            }
        }
    }
}

/// The Verdict X; `lower` keeps the counter-stroke's lower segment (only the
/// idle mark has it — badges take its corner).
fn mark(x: f64, y: f64, lower: bool) -> bool {
    let p = (x, y);
    seg_dist(p, CHECK[0], CHECK[1]) <= CHECK_HALF_W
        || seg_dist(p, CHECK[1], CHECK[2]) <= CHECK_HALF_W
        || seg_dist(p, COUNTER_UPPER[0], COUNTER_UPPER[1]) <= COUNTER_HALF_W
        || (lower && seg_dist(p, COUNTER_LOWER[0], COUNTER_LOWER[1]) <= COUNTER_HALF_W)
}

fn dist(a: Pt, b: Pt) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Inside the rounded rectangle `(x, y, w, h)` with corner radius `r`.
fn in_round_rect(px: f64, py: f64, (x, y, w, h): (f64, f64, f64, f64), r: f64) -> bool {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) <= r
}

fn seg_dist(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0)
    };
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

// ------------------------------------------------------------- AppKit

/// One 18x18 pt template `NSImage` carrying an 18/36/54 px bitmap
/// representation per [`SCALES`] entry, so AppKit picks the backing scale's
/// own raster instead of upscaling. AppKit tints the template for the menu
/// bar (light, dark, tinted, highlighted).
pub fn nsimage(state: IconState) -> Retained<NSImage> {
    let size = NSSize::new(f64::from(SIZE), f64::from(SIZE));
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    for scale in SCALES {
        let side = (SIZE * scale) as isize;
        let pixels = rgba(state, scale);
        // SAFETY: null planes ask AppKit to allocate its own meshed buffer of
        // `side * side * 4` bytes (8-bit RGBA, explicit row stride), which
        // `pixels` fills exactly.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                side,
                side,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat::AlphaNonpremultiplied,
                side * 4,
                32,
            )
        }
        .expect("bitmap rep geometry is valid");
        let data = rep.bitmapData();
        assert!(!data.is_null(), "bitmap rep allocated its buffer");
        // SAFETY: `data` points at the rep's own `side * side * 4`-byte buffer.
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), data, pixels.len()) };
        rep.setSize(size);
        image.addRepresentation(&rep);
    }
    image.setTemplate(true);
    image
}

/// The status-bar button's effective appearance (the menu bar's, which can
/// differ from the app's). Light when it cannot be determined.
pub fn appearance_of(tray: &tray_icon::TrayIcon) -> Appearance {
    let Some(mtm) = MainThreadMarker::new() else {
        return Appearance::Light;
    };
    let Some(button) = tray.ns_status_item().and_then(|item| item.button(mtm)) else {
        return Appearance::Light;
    };
    // SAFETY: AppKit's appearance-name constants are immutable statics.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let names = NSArray::from_slice(&[aqua, dark]);
    match button
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&names)
    {
        Some(name) if name.isEqualToString(dark) => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Put the multi-scale template image for `state` on the tray's status-bar
/// button (bypassing `tray_icon::Icon`, which can only express one raster).
/// The point size matches the 1x icon the tray was built with, so
/// tray-icon's click-tracking geometry stays valid. `_appearance` is unused:
/// AppKit tints templates itself.
pub fn apply(
    tray: &tray_icon::TrayIcon,
    state: IconState,
    _appearance: Appearance,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("icon updates must run on the main thread")?;
    let button = tray
        .ns_status_item()
        .and_then(|item| item.button(mtm))
        .ok_or("status item has no button")?;
    button.setImage(Some(&nsimage(state)));
    Ok(())
}

/// Set the status item's count title and tooltip (see [`count_title`] and
/// [`tooltip`]). tray-icon keeps the image left of the title and resizes
/// its click-tracking area to the new width.
pub fn apply_title(tray: &tray_icon::TrayIcon, title: Option<&str>, tooltip: &str) {
    tray.set_title(title);
    if let Err(error) = tray.set_tooltip(Some(tooltip)) {
        eprintln!("prx-menubar: cannot set the tooltip: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [IconState; 5] = [Idle, Attention, Reviewing, Paused, Offline];

    fn pixels(rgba: &[u8]) -> &[[u8; 4]] {
        rgba.as_chunks::<4>().0
    }

    /// Mean alpha over the canvas (0..=1): the ink coverage.
    fn coverage(rgba: &[u8]) -> f64 {
        let px = pixels(rgba);
        px.iter().map(|p| f64::from(p[3])).sum::<f64>() / (px.len() as f64 * 255.0)
    }

    /// Exact (unrasterized) ink at a point in 18-point space.
    fn ink(state: IconState, p: Pt) -> f64 {
        sample(p.0, p.1, state)
    }

    #[test]
    fn every_state_renders_at_every_scale() {
        for state in ALL {
            for scale in SCALES {
                let rgba = rgba(state, scale);
                let side = (SIZE * scale) as usize;
                assert_eq!(rgba.len(), side * side * 4, "{state:?} @{scale}x");
                assert!(
                    pixels(&rgba).iter().any(|px| px[3] > 0),
                    "{state:?} @{scale}x must draw something"
                );
            }
        }
    }

    /// Template pixels: black ink only, the shape lives in alpha — so no
    /// state can ever come out red (or any colour): AppKit tints it.
    #[test]
    fn pixels_are_template_ink() {
        for state in ALL {
            assert!(
                pixels(&rgba(state, 2))
                    .iter()
                    .all(|px| px[..3] == [0, 0, 0]),
                "{state:?}"
            );
        }
    }

    /// The same geometry at every scale: ink coverage agrees across 1x/2x/3x
    /// (only anti-aliasing differs), so the retina reps are the 1x art drawn
    /// finer, not different art.
    #[test]
    fn geometry_is_scale_invariant() {
        for state in ALL {
            let base = coverage(&rgba(state, 1));
            for scale in [2, 3] {
                let scaled = coverage(&rgba(state, scale));
                assert!(
                    (scaled - base).abs() < 0.01,
                    "{state:?}: coverage {base:.4} @1x vs {scaled:.4} @{scale}x"
                );
            }
        }
    }

    #[test]
    fn higher_scales_carry_more_detail() {
        // A 3x rep has more distinct alpha levels along edges than 1x: it is
        // rendered, not upscaled (an upscaled raster repeats each value 3x3).
        let one = rgba(Idle, 1);
        let three = rgba(Idle, 3);
        let upscaled_equal = (0..SIZE as usize * 3).all(|y| {
            (0..SIZE as usize * 3).all(|x| {
                let hi = three[(y * SIZE as usize * 3 + x) * 4 + 3];
                let lo = one[((y / 3) * SIZE as usize + x / 3) * 4 + 3];
                hi == lo
            })
        });
        assert!(!upscaled_equal, "3x must not be a nearest-neighbor upscale");
    }

    /// Monochrome distinctness: alpha is the only channel, so differing
    /// rasters means differing shapes (the grayscale requirement). The
    /// badge corners must differ clearly, not by a pixel: compare the ink
    /// inside the badge zone at 1x, where it is hardest.
    #[test]
    fn states_are_visually_distinct() {
        for scale in SCALES {
            for (i, a) in ALL.iter().enumerate() {
                for b in &ALL[i + 1..] {
                    assert_ne!(rgba(*a, scale), rgba(*b, scale), "{a:?} vs {b:?} @{scale}x");
                }
            }
        }
        let corner = |state: IconState| -> Vec<f64> {
            let px = rgba(state, 1);
            (10..18)
                .flat_map(|y| (10..18).map(move |x| (x, y)))
                .map(|(x, y)| f64::from(px[(y * SIZE as usize + x) * 4 + 3]) / 255.0)
                .collect()
        };
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                let (ca, cb) = (corner(*a), corner(*b));
                let diff: f64 = ca.iter().zip(&cb).map(|(x, y)| (x - y).abs()).sum();
                assert!(
                    diff > 4.0,
                    "{a:?} vs {b:?}: corners differ by only {diff:.2} px"
                );
            }
        }
    }

    /// The idle mark keeps both counter-stroke segments with the gap where
    /// it would cross the check; every badged state gives up the lower
    /// segment's corner.
    #[test]
    fn idle_mark_is_the_interrupted_verdict_x() {
        let gap = (8.6, 8.6); // on the diagonal, between the upper segment and the check
        let upper = (5.0, 5.0);
        let lower = (12.5, 12.5);
        let vertex = CHECK[1];
        for state in ALL {
            assert_eq!(ink(state, gap), 0.0, "{state:?} keeps the gap");
            assert!(ink(state, upper) > 0.0, "{state:?} keeps the upper segment");
            assert!(
                ink(state, vertex) > 0.0,
                "{state:?} keeps the check's vertex"
            );
        }
        assert_eq!(ink(Idle, lower), 1.0);
        assert!(
            !mark(lower.0, lower.1, false),
            "badged marks drop the lower segment"
        );
        assert_eq!(ink(Reviewing, lower), 0.0);
    }

    /// Badges stand apart from the mark: nothing is inked in the clearance
    /// band between the badge (r <= 4) and the knock-out (r = 5).
    #[test]
    fn badges_are_knocked_out_of_the_mark() {
        for state in [Attention, Reviewing, Paused, Offline] {
            for i in 0..360 {
                let a = f64::from(i).to_radians();
                for r in [4.1, 4.5, 4.9] {
                    let p = (BADGE_CENTER.0 + r * a.cos(), BADGE_CENTER.1 + r * a.sin());
                    assert_eq!(ink(state, p), 0.0, "{state:?} at r={r} {i}°");
                }
            }
        }
    }

    #[test]
    fn badge_shapes_differ() {
        // Attention is a solid dot: inked at its center and out to r = 3.
        assert_eq!(ink(Attention, BADGE_CENTER), 1.0);
        assert_eq!(ink(Attention, (14.0, 11.0)), 1.0, "dot is solid to its rim");
        assert_eq!(ink(Attention, (11.2, 14.0)), 1.0);
        // Reviewing: three separate dots — the gaps between them are empty.
        assert_eq!(ink(Reviewing, BADGE_CENTER), 1.0);
        assert_eq!(ink(Reviewing, (12.6, 14.0)), 0.0, "gap between dots");
        assert_eq!(ink(Reviewing, (14.0, 11.0)), 0.0, "no disc");
        // Paused: bars with an empty middle.
        assert_eq!(ink(Paused, BADGE_CENTER), 0.0);
        assert_eq!(ink(Paused, (12.0, 14.0)), 1.0);
        // Offline: a hollow ring with a slash through it.
        assert_eq!(ink(Offline, BADGE_CENTER), 1.0, "slash");
        assert_eq!(ink(Offline, (14.0, 14.0 - RING_R)), 1.0, "ring");
        assert_eq!(ink(Offline, (15.5, 12.5)), 0.0, "hollow");
        assert_eq!(ink(Attention, (15.5, 12.5)), 1.0, "solid");
    }

    #[test]
    fn offline_dims_the_mark() {
        assert_eq!(ink(Offline, CHECK[1]), OFFLINE_MARK_ALPHA);
        assert_eq!(ink(Idle, CHECK[1]), 1.0);
    }

    /// `assets/menubar-glyph.svg` is the master: the constants here must
    /// match its paths, so the shipped pixels and the SVG cannot drift.
    #[test]
    fn svg_master_matches_the_geometry() {
        let svg = include_str!("../../assets/menubar-glyph.svg");
        let n = |v: f64| {
            let s = format!("{v:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        let p = |(x, y): Pt| format!("{} {}", n(x), n(y));
        let expected = [
            format!(
                "<path d=\"M {} L {}\" stroke-width=\"{}\"/>",
                p(COUNTER_UPPER[0]),
                p(COUNTER_UPPER[1]),
                n(COUNTER_HALF_W * 2.0)
            ),
            format!(
                "<path d=\"M {} L {}\" stroke-width=\"{}\"/>",
                p(COUNTER_LOWER[0]),
                p(COUNTER_LOWER[1]),
                n(COUNTER_HALF_W * 2.0)
            ),
            format!(
                "<path d=\"M {} L {} L {}\" stroke-width=\"{}\"/>",
                p(CHECK[0]),
                p(CHECK[1]),
                p(CHECK[2]),
                n(CHECK_HALF_W * 2.0)
            ),
            format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"#000\"/>",
                n(BADGE_CENTER.0),
                n(BADGE_CENTER.1),
                n(BADGE_CLEAR_R)
            ),
            format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"#000\" class=\"attention\"/>",
                n(BADGE_CENTER.0),
                n(BADGE_CENTER.1),
                n(ATTENTION_R)
            ),
            format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\"/>",
                n(BADGE_CENTER.0),
                n(BADGE_CENTER.1),
                n(RING_R)
            ),
            format!(
                "stroke-width=\"{}\" stroke-linecap=\"round\"><circle",
                n(OFFLINE_HALF_W * 2.0)
            ),
            format!("<path d=\"M {} L {}\"/>", p(SLASH[0]), p(SLASH[1])),
            format!("opacity=\"{}\"", n(OFFLINE_MARK_ALPHA)),
        ]
        .into_iter()
        .chain(DOTS.iter().map(|&(x, y)| {
            format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\"/>",
                n(x),
                n(y),
                n(DOT_R)
            )
        }))
        .chain(BARS.iter().map(|&(x, y, w, h)| {
            format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{}\"/>",
                n(x),
                n(y),
                n(w),
                n(h),
                n(BAR_RADIUS)
            )
        }));
        for snippet in expected {
            assert!(svg.contains(&snippet), "menubar-glyph.svg lacks {snippet}");
        }
        for id in ["idle", "attention", "reviewing", "paused", "offline"] {
            assert!(svg.contains(&format!("<g id=\"{id}\"")), "state group {id}");
        }
        assert!(
            !svg.contains("<text"),
            "the count is the status item's title, never drawn in the glyph"
        );
    }

    fn model(
        offline: bool,
        stopped: bool,
        paused: bool,
        in_flight: usize,
        attention: usize,
    ) -> crate::model::MenuModel {
        crate::model::MenuModel {
            offline,
            attention,
            in_flight,
            stopped,
            paused,
            sections: Vec::new(),
        }
    }

    #[test]
    fn icon_state_precedence() {
        let state = |o, s, p, f, a| IconState::for_model(&model(o, s, p, f, a));
        assert_eq!(state(true, true, true, 2, 3), Offline);
        assert_eq!(state(false, true, false, 2, 3), Paused, "review mode off");
        assert_eq!(state(false, false, true, 2, 3), Paused, "a pause");
        assert_eq!(state(false, false, false, 2, 3), Reviewing);
        assert_eq!(state(false, false, false, 0, 3), Attention);
        assert_eq!(state(false, false, false, 0, 0), Idle);
    }

    /// The count lives in the title, beside every online glyph state.
    #[test]
    fn count_title_shows_attention_while_online() {
        let title = |o, s, p, f, a| count_title(&model(o, s, p, f, a));
        assert_eq!(title(false, false, false, 0, 0), None, "idle: glyph only");
        assert_eq!(title(false, false, false, 0, 4).as_deref(), Some("4"));
        assert_eq!(
            title(false, false, false, 2, 4).as_deref(),
            Some("4"),
            "reviewing keeps it"
        );
        assert_eq!(
            title(false, false, true, 0, 4).as_deref(),
            Some("4"),
            "paused keeps it"
        );
        assert_eq!(title(false, false, false, 0, 99).as_deref(), Some("99"));
        assert_eq!(title(false, false, false, 0, 137).as_deref(), Some("99+"));
        assert_eq!(
            title(true, false, false, 0, 4),
            None,
            "offline: no stale count"
        );
    }

    #[test]
    fn tooltip_says_the_state_in_words() {
        assert_eq!(
            tooltip(&model(false, false, false, 0, 0)),
            "PRX · all clear"
        );
        assert_eq!(
            tooltip(&model(false, false, false, 1, 4)),
            "PRX · 4 PRs need attention · reviewing"
        );
        assert_eq!(
            tooltip(&model(false, false, true, 0, 1)),
            "PRX · 1 PR needs attention · reviews paused"
        );
        assert_eq!(
            tooltip(&model(false, true, false, 0, 0)),
            "PRX · all clear · reviews off"
        );
        assert_eq!(
            tooltip(&model(true, false, false, 0, 9)),
            "PRX · daemon offline"
        );
    }

    /// The AppKit image: one 18x18 pt template NSImage with an 18/36/54 px
    /// rep each.
    #[test]
    fn nsimage_is_a_multi_scale_template() {
        for state in [Idle, Reviewing, Attention] {
            let image = nsimage(state);
            let size = image.size();
            assert_eq!((size.width, size.height), (18.0, 18.0));
            assert!(image.isTemplate(), "template, tinted by AppKit");
            let reps = image.representations();
            let mut widths: Vec<isize> = reps.iter().map(|rep| rep.pixelsWide()).collect();
            widths.sort_unstable();
            assert_eq!(widths, [18, 36, 54], "{state:?}");
            for rep in reps.iter() {
                assert_eq!(rep.size().width, 18.0, "every rep is 18 pt");
            }
        }
    }

    use IconState::{Attention, Idle, Offline, Paused, Reviewing};
}
