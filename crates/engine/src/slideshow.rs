//! Slideshow playback: the settings of Lightroom Classic's Playback panel and the timeline that
//! plays the photos in view (durations, fades, random order, repeat, pause, stepping).
//!
//! No UI here: [`Playback`] is a pure state machine driven with injected times (seconds on any
//! monotonic clock), so the desktop app, a later Slideshow module and tests run the same timeline.
//! The frontend asks [`Playback::frame`] what to draw (the current slide, and while a transition
//! runs the previous one and its progress) and [`Playback::upcoming`] which slides to prepare; it
//! tells [`Playback::tick`] which slides are ready, so a transition never starts on a slide that
//! has not been rendered yet (the current one is held instead).
//!
//! The settings are a library preference (`prefs.json`, see [`crate::library`]), read and changed
//! with the `slideshow.playback` command.

use lightcraft_catalog::PhotoId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Shortest and longest time a slide stays up (seconds).
pub const SLIDE_MIN: f64 = 0.5;
pub const SLIDE_MAX: f64 = 120.0;
/// Longest fade (seconds); a fade is never longer than the slide duration.
pub const FADE_MAX: f64 = 10.0;

/// How playback runs (Lightroom Classic's Playback panel, without music and pan & zoom).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaybackSettings {
    /// Seconds from the start of one slide's transition to the start of the next one's (the
    /// fade in is part of it).
    pub slide_duration: f64,
    /// Seconds a transition takes (0 = cut).
    pub fade_duration: f64,
    /// Fade through this colour (out to it, then in from it) instead of crossfading.
    pub fade_color: Option<[u8; 3]>,
    /// Shuffle the photos (a new order for every pass when repeating).
    pub random_order: bool,
    /// Start over after the last slide; off: playback ends there.
    pub repeat: bool,
    /// Render the next slides before they are due.
    pub prepare_previews: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        PlaybackSettings { slide_duration: 4.0, fade_duration: 1.0, fade_color: None, random_order: false, repeat: true, prepare_previews: true }
    }
}

impl PlaybackSettings {
    /// The settings with every value in range (a non-finite duration takes the default; the fade
    /// is never longer than the slide).
    pub fn sanitized(mut self) -> Self {
        let d = PlaybackSettings::default();
        self.slide_duration = if self.slide_duration.is_finite() { self.slide_duration.clamp(SLIDE_MIN, SLIDE_MAX) } else { d.slide_duration };
        let fade = if self.fade_duration.is_finite() { self.fade_duration } else { d.fade_duration };
        self.fade_duration = fade.clamp(0.0, FADE_MAX).min(self.slide_duration);
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "slideDuration": self.slide_duration,
            "fadeDuration": self.fade_duration,
            "fadeColor": self.fade_color.map(hex),
            "randomOrder": self.random_order,
            "repeat": self.repeat,
            "preparePreviews": self.prepare_previews,
        })
    }

    /// These settings with the keys `p` names changed (the others kept), validated and clamped.
    /// A wrong type, an unknown key or a non-finite number is an error; nothing changes then.
    pub fn merged(&self, p: &Value) -> std::result::Result<Self, String> {
        let Some(obj) = p.as_object() else { return Err("parameters must be an object".into()) };
        let mut s = self.clone();
        for (k, v) in obj {
            match k.as_str() {
                "slideDuration" => s.slide_duration = finite(k, v)?,
                "fadeDuration" => s.fade_duration = finite(k, v)?,
                "fadeColor" => s.fade_color = color(v)?,
                "randomOrder" => s.random_order = boolean(k, v)?,
                "repeat" => s.repeat = boolean(k, v)?,
                "preparePreviews" => s.prepare_previews = boolean(k, v)?,
                other => {
                    return Err(format!("unknown setting `{other}` (slideDuration, fadeDuration, fadeColor, randomOrder, repeat, preparePreviews)"));
                }
            }
        }
        // a fade longer than a slide asked for together with a shorter slide: the slide wins
        Ok(s.sanitized())
    }
}

fn finite(k: &str, v: &Value) -> std::result::Result<f64, String> {
    match v.as_f64() {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(format!("`{k}` must be a finite number of seconds, not {v}")),
    }
}

fn boolean(k: &str, v: &Value) -> std::result::Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("`{k}` must be true or false, not {v}"))
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `null` / `""` / `"none"` = crossfade; `"#rrggbb"` or `[r, g, b]` (0–255) = fade through it.
fn color(v: &Value) -> std::result::Result<Option<[u8; 3]>, String> {
    let bad = || format!("`fadeColor` must be \"#rrggbb\", [r, g, b] or null, not {v}");
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() || s.eq_ignore_ascii_case("none") => Ok(None),
        Value::String(s) => {
            let h = s.strip_prefix('#').unwrap_or(s);
            if h.len() != 6 || !h.is_ascii() {
                return Err(bad());
            }
            let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).ok_or_else(bad);
            Ok(Some([byte(0)?, byte(2)?, byte(4)?]))
        }
        Value::Array(a) if a.len() == 3 => {
            let mut c = [0u8; 3];
            for (o, x) in c.iter_mut().zip(a) {
                *o = x.as_u64().filter(|n| *n <= 255).ok_or_else(bad)? as u8;
            }
            Ok(Some(c))
        }
        _ => Err(bad()),
    }
}

/// What a tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Nothing changed (the slide is up, paused, or a transition is running).
    Idle,
    /// The next slide is due but not ready: the current one is held.
    Waiting(PhotoId),
    /// A new slide came up (its transition starts now).
    Advanced(PhotoId),
    /// The last slide was shown and repeat is off.
    Ended,
}

/// What to draw at a moment of playback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub current: PhotoId,
    /// The slide being faded out, while a transition runs.
    pub from: Option<PhotoId>,
    /// How far the transition is (0–1; 1 when there is none).
    pub progress: f32,
    pub fade_color: Option<[u8; 3]>,
}

/// One run of a slideshow over a fixed list of photos.
#[derive(Clone, Debug)]
pub struct Playback {
    settings: PlaybackSettings,
    photos: Vec<PhotoId>,
    /// This pass's order (indices into `photos`).
    order: Vec<usize>,
    /// Index into `order` of the slide up now.
    pos: usize,
    pass: u64,
    seed: u64,
    /// When the current slide came up (its transition started).
    started: f64,
    /// The slide fading out (until the transition ends).
    from: Option<PhotoId>,
    /// Paused at this time.
    paused_at: Option<f64>,
    ended: bool,
    /// The next slide was due but not ready at the last tick.
    held: Option<PhotoId>,
}

/// SplitMix64: a small, well-mixed generator for the shuffle (deterministic per seed).
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Fisher–Yates shuffle of `v` with `seed`.
fn shuffle(v: &mut [usize], seed: u64) {
    let mut st = seed;
    for i in (1..v.len()).rev() {
        let j = (splitmix(&mut st) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

impl Playback {
    /// Start playing `photos` (the ones in view, in view order) at `start` (the selected photo;
    /// the first one when absent or not among them), at time `now`. `seed` fixes the random order.
    /// `None` when there is nothing to show.
    pub fn new(photos: Vec<PhotoId>, start: Option<PhotoId>, settings: PlaybackSettings, seed: u64, now: f64) -> Option<Self> {
        if photos.is_empty() {
            return None;
        }
        let settings = settings.sanitized();
        let n = photos.len();
        let first = start.and_then(|s| photos.iter().position(|p| *p == s)).unwrap_or(0);
        let order: Vec<usize> = if settings.random_order {
            // the starting photo first, the others shuffled
            let mut rest: Vec<usize> = (0..n).filter(|i| *i != first).collect();
            shuffle(&mut rest, seed);
            std::iter::once(first).chain(rest).collect()
        } else {
            // from the starting photo to the end, then the ones before it: every photo once
            (first..n).chain(0..first).collect()
        };
        let now = if now.is_finite() { now } else { 0.0 };
        Some(Playback { settings, photos, order, pos: 0, pass: 0, seed, started: now, from: None, paused_at: None, ended: false, held: None })
    }

    pub fn settings(&self) -> &PlaybackSettings {
        &self.settings
    }

    /// The order of pass `pass` (after the first), given the order of the pass before it.
    fn order_after(&self, pass: u64, prev: &[usize]) -> Vec<usize> {
        let mut order = prev.to_vec();
        if self.settings.random_order {
            order.sort_unstable();
            shuffle(&mut order, self.seed ^ pass.wrapping_mul(0xa076_1d64_78bd_642f));
            // never the same photo twice in a row across passes
            if order.len() > 1 && order.first() == prev.last() {
                order.swap(0, 1);
            }
        }
        order
    }

    fn photo_at(&self, order: &[usize], pos: usize) -> Option<PhotoId> {
        order.get(pos).and_then(|i| self.photos.get(*i)).copied()
    }

    /// The slide up now.
    pub fn current(&self) -> PhotoId {
        self.photo_at(&self.order, self.pos).or_else(|| self.photos.first().copied()).unwrap_or(PhotoId(0))
    }

    /// (slide number from 1, slides per pass).
    pub fn position(&self) -> (usize, usize) {
        (self.pos + 1, self.order.len())
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// The slide that was due but not ready at the last tick (the current one is held for it).
    pub fn waiting_for(&self) -> Option<PhotoId> {
        self.held
    }

    /// The clock as playback sees it: stopped while paused.
    fn clock(&self, now: f64) -> f64 {
        let now = if now.is_finite() { now } else { self.started };
        self.paused_at.map_or(now, |p| p.min(now))
    }

    /// When the next slide is due.
    pub fn due(&self) -> f64 {
        self.started + self.settings.slide_duration
    }

    /// Seconds until the next slide is due (as of `now`; frozen while paused).
    pub fn remaining(&self, now: f64) -> f64 {
        (self.due() - self.clock(now)).max(0.0)
    }

    /// The next `n` slides in playing order (into the next pass when repeating; never the one up
    /// now twice).
    pub fn upcoming(&self, n: usize) -> Vec<PhotoId> {
        let mut out = Vec::new();
        let mut order = self.order.clone();
        let (mut pos, mut pass) = (self.pos, self.pass);
        while out.len() < n {
            pos += 1;
            if pos >= order.len() {
                if !self.settings.repeat {
                    break;
                }
                pass += 1;
                order = self.order_after(pass, &order);
                pos = 0;
            }
            match self.photo_at(&order, pos) {
                Some(p) if p != self.current() && !out.contains(&p) => out.push(p),
                _ => break,
            }
        }
        out
    }

    /// Move one slide on (`Advanced`), or end (`Ended`) past the last one without repeat.
    fn advance(&mut self) -> Tick {
        if self.pos + 1 < self.order.len() {
            self.pos += 1;
        } else if self.settings.repeat {
            self.pass += 1;
            self.order = self.order_after(self.pass, &self.order);
            self.pos = 0;
        } else {
            self.ended = true;
            return Tick::Ended;
        }
        Tick::Advanced(self.current())
    }

    /// Advance the timeline to `now`. `ready(photo)` says whether a slide can be shown: the
    /// transition to the next slide waits until it is.
    pub fn tick(&mut self, now: f64, ready: impl Fn(PhotoId) -> bool) -> Tick {
        if self.ended {
            return Tick::Ended;
        }
        let now = self.clock(now);
        if self.from.is_some() && now >= self.started + self.settings.fade_duration {
            self.from = None;
        }
        if self.paused_at.is_some() {
            return Tick::Idle;
        }
        // a slide's time starts once it can be shown (the first one, one stepped to with ←/→)
        if self.from.is_none() && !ready(self.current()) {
            self.started = now;
            return Tick::Idle;
        }
        if now < self.due() {
            return Tick::Idle;
        }
        let Some(next) = self.upcoming(1).first().copied() else {
            if self.settings.repeat {
                // a single photo repeating: nothing to change to
                self.started = now;
                return Tick::Idle;
            }
            self.ended = true;
            return Tick::Ended;
        };
        if !ready(next) {
            self.held = Some(next);
            return Tick::Waiting(next);
        }
        self.held = None;
        let old = self.current();
        let t = self.advance();
        if let Tick::Advanced(_) = t {
            self.started = now;
            self.from = (self.settings.fade_duration > 0.0).then_some(old);
        }
        t
    }

    /// What to draw at `now`.
    pub fn frame(&self, now: f64) -> Frame {
        let now = self.clock(now);
        let fade = self.settings.fade_duration;
        let (from, progress) = match self.from {
            Some(f) if fade > 0.0 && now < self.started + fade => (Some(f), ((now - self.started) / fade).clamp(0.0, 1.0) as f32),
            _ => (None, 1.0),
        };
        Frame { current: self.current(), from, progress, fade_color: self.settings.fade_color }
    }

    /// Pause or resume at `now`; resuming keeps the time the slide had left. Returns whether it
    /// is paused now.
    pub fn toggle_pause(&mut self, now: f64) -> bool {
        let now = if now.is_finite() { now } else { self.started };
        match self.paused_at.take() {
            Some(p) => {
                // the slide (and a transition in progress) carries on where it stopped
                self.started += (now - p).max(0.0);
            }
            None => self.paused_at = Some(now),
        }
        self.paused_at.is_some()
    }

    /// Step `delta` slides (←/→) at `now`: a cut, and the new slide gets its full time (paused
    /// playback stays paused). Past the last slide without repeat playback ends; before the first
    /// it stays there.
    pub fn step(&mut self, delta: i32, now: f64) -> Tick {
        if self.ended {
            return Tick::Ended;
        }
        let now = if now.is_finite() { now } else { self.started };
        // (more steps than slides would only go round again)
        let steps = delta.unsigned_abs().min(self.order.len().min(u32::MAX as usize) as u32);
        let mut t = Tick::Idle;
        if delta > 0 {
            for _ in 0..steps {
                t = self.advance();
                if t == Tick::Ended {
                    return t;
                }
            }
        } else {
            for _ in 0..steps {
                if self.pos > 0 {
                    self.pos -= 1;
                } else if self.settings.repeat {
                    self.pos = self.order.len().saturating_sub(1);
                }
            }
            t = Tick::Advanced(self.current());
        }
        self.from = None;
        self.held = None;
        self.started = now;
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u64) -> Vec<PhotoId> {
        (1..=n).map(PhotoId).collect()
    }

    fn settings(slide: f64, fade: f64) -> PlaybackSettings {
        PlaybackSettings { slide_duration: slide, fade_duration: fade, ..Default::default() }
    }

    fn all_ready(_: PhotoId) -> bool {
        true
    }

    #[test]
    fn timeline_durations_and_fade_progress() {
        let mut p = Playback::new(ids(3), Some(PhotoId(2)), settings(4.0, 1.0), 1, 10.0).unwrap();
        assert_eq!(p.current(), PhotoId(2), "starts at the selected photo");
        assert_eq!(p.position(), (1, 3));
        assert_eq!(p.tick(13.9, all_ready), Tick::Idle);
        assert_eq!(p.frame(13.9).from, None);
        assert_eq!(p.tick(14.0, all_ready), Tick::Advanced(PhotoId(3)));
        let f = p.frame(14.25);
        assert_eq!((f.current, f.from), (PhotoId(3), Some(PhotoId(2))));
        assert!((f.progress - 0.25).abs() < 1e-6, "{f:?}");
        assert!((p.frame(14.5).progress - 0.5).abs() < 1e-6);
        assert_eq!(p.tick(15.0, all_ready), Tick::Idle);
        assert_eq!(p.frame(15.0).from, None, "the fade is over after fadeDuration");
        // the slide period counts from its transition: next at 18
        assert_eq!(p.tick(17.99, all_ready), Tick::Idle);
        assert_eq!(p.tick(18.0, all_ready), Tick::Advanced(PhotoId(1)), "wraps to the photos before the start");
        assert_eq!(p.position(), (3, 3));
    }

    #[test]
    fn fade_zero_is_a_cut_and_fade_color_is_reported() {
        let mut p = Playback::new(ids(2), None, settings(2.0, 0.0), 1, 0.0).unwrap();
        assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(PhotoId(2)));
        assert_eq!(p.frame(2.0).from, None);
        let s = PlaybackSettings { fade_color: Some([255, 0, 0]), ..settings(2.0, 1.0) };
        let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
        p.tick(2.0, all_ready);
        assert_eq!(p.frame(2.5).fade_color, Some([255, 0, 0]));
    }

    #[test]
    fn arrows_cut_immediately_and_restart_the_slide_time() {
        let mut p = Playback::new(ids(4), None, settings(4.0, 2.0), 1, 0.0).unwrap();
        p.tick(4.0, all_ready);
        assert!(p.frame(4.5).from.is_some(), "fading");
        assert_eq!(p.step(1, 4.5), Tick::Advanced(PhotoId(3)));
        assert_eq!(p.frame(4.5), Frame { current: PhotoId(3), from: None, progress: 1.0, fade_color: None }, "a cut");
        assert_eq!(p.tick(8.4, all_ready), Tick::Idle, "full time from the step");
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(PhotoId(2)));
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(PhotoId(1)));
        // repeat on: before the first is the last
        assert_eq!(p.step(-1, 6.0), Tick::Advanced(PhotoId(4)));
        assert_eq!(p.remaining(6.0), 4.0);
    }

    #[test]
    fn random_order_is_deterministic_per_seed_and_covers_every_photo_once_per_pass() {
        let photos = ids(20);
        let run = |seed: u64| {
            let s = PlaybackSettings { random_order: true, ..settings(1.0, 0.0) };
            let mut p = Playback::new(photos.clone(), Some(PhotoId(5)), s, seed, 0.0).unwrap();
            let mut seen = vec![p.current()];
            for k in 1..60 {
                p.tick(k as f64, all_ready);
                seen.push(p.current());
            }
            seen
        };
        let a = run(42);
        assert_eq!(a, run(42), "same seed, same order");
        assert_ne!(a, run(43), "another seed, another order");
        assert_eq!(a[0], PhotoId(5), "starts at the selected photo");
        assert_ne!(a[..20].to_vec(), photos, "shuffled");
        for pass in a.chunks(20).take(2) {
            let mut sorted = pass.to_vec();
            sorted.sort();
            assert_eq!(sorted, photos, "every photo once per pass");
        }
        assert_ne!(a[..20], a[20..40], "a new order for the next pass");
        assert!(a.windows(2).all(|w| w[0] != w[1]), "never the same photo twice in a row");
    }

    #[test]
    fn repeat_off_stops_after_the_last_slide() {
        let s = PlaybackSettings { repeat: false, ..settings(1.0, 0.0) };
        let mut p = Playback::new(ids(3), Some(PhotoId(2)), s.clone(), 1, 0.0).unwrap();
        assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(PhotoId(3)));
        assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(PhotoId(1)));
        assert_eq!(p.upcoming(2), vec![], "nothing after the last");
        assert_eq!(p.tick(3.0, all_ready), Tick::Ended);
        assert!(p.is_ended());
        // → past the last one ends it too
        let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
        assert_eq!(p.step(1, 0.5), Tick::Advanced(PhotoId(2)));
        assert_eq!(p.step(1, 0.6), Tick::Ended);
        // with repeat: it goes on
        let mut p = Playback::new(ids(2), None, settings(1.0, 0.0), 1, 0.0).unwrap();
        for k in 1..10 {
            assert!(matches!(p.tick(k as f64, all_ready), Tick::Advanced(_)));
        }
    }

    #[test]
    fn pause_and_resume_keep_the_remaining_time() {
        let mut p = Playback::new(ids(3), None, settings(4.0, 1.0), 1, 0.0).unwrap();
        assert!(p.toggle_pause(3.0));
        assert_eq!(p.tick(100.0, all_ready), Tick::Idle, "paused: nothing moves");
        assert_eq!(p.remaining(100.0), 1.0);
        assert!(!p.toggle_pause(100.0));
        assert_eq!(p.tick(100.9, all_ready), Tick::Idle);
        assert_eq!(p.tick(101.0, all_ready), Tick::Advanced(PhotoId(2)), "1 s left after resuming");
        // pausing mid-fade freezes the fade
        p.toggle_pause(101.25);
        assert!((p.frame(150.0).progress - 0.25).abs() < 1e-6);
        p.toggle_pause(150.0);
        assert!((p.frame(150.5).progress - 0.75).abs() < 1e-6);
    }

    #[test]
    fn a_transition_waits_for_the_next_slide_to_be_ready() {
        let mut p = Playback::new(ids(3), None, settings(2.0, 1.0), 1, 0.0).unwrap();
        assert_eq!(p.upcoming(2), vec![PhotoId(2), PhotoId(3)], "what to prepare");
        let not_two = |id: PhotoId| id != PhotoId(2);
        assert_eq!(p.tick(2.0, not_two), Tick::Waiting(PhotoId(2)));
        assert_eq!(p.frame(3.0), Frame { current: PhotoId(1), from: None, progress: 1.0, fade_color: None }, "held, not flashed");
        assert_eq!(p.waiting_for(), Some(PhotoId(2)));
        assert_eq!(p.tick(3.5, all_ready), Tick::Advanced(PhotoId(2)));
        assert_eq!(p.frame(3.5).from, Some(PhotoId(1)), "the fade starts once it is ready");
        assert!((p.frame(4.0).progress - 0.5).abs() < 1e-6, "and runs its full length");
    }

    #[test]
    fn a_slide_s_time_starts_once_it_is_ready() {
        let mut p = Playback::new(ids(3), None, settings(2.0, 0.5), 1, 0.0).unwrap();
        let none = |_: PhotoId| false;
        assert_eq!(p.tick(1.0, none), Tick::Idle);
        assert_eq!(p.tick(5.0, none), Tick::Idle, "the first slide isn't rendered yet: its clock waits");
        assert_eq!(p.remaining(5.0), 2.0);
        assert_eq!(p.tick(6.0, all_ready), Tick::Idle);
        assert_eq!(p.tick(7.0, all_ready), Tick::Advanced(PhotoId(2)), "2 s from when it could be shown");
    }

    #[test]
    fn hostile_settings_are_rejected_or_clamped() {
        let d = PlaybackSettings::default();
        for bad in [
            json!({"slideDuration": "fast"}),
            json!({"fadeDuration": null}),
            json!({"repeat": 1}),
            json!({"fadeColor": "#12345"}),
            json!({"fadeColor": "#ééé"}),
            json!({"fadeColor": [300, 0, 0]}),
            json!({"fadeColor": [1, 2]}),
            json!({"speed": 2}),
            json!([1, 2]),
        ] {
            assert!(d.merged(&bad).is_err(), "{bad}");
        }
        let s = d.merged(&json!({"slideDuration": 1e300, "fadeDuration": -5})).unwrap();
        assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MAX, 0.0));
        let s = d.merged(&json!({"slideDuration": 0.0, "fadeDuration": 9})).unwrap();
        assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MIN, SLIDE_MIN), "the fade fits in the slide");
        let s = d.merged(&json!({"fadeColor": "#FF8000"})).unwrap();
        assert_eq!(s.fade_color, Some([255, 128, 0]));
        assert_eq!(s.to_json()["fadeColor"], "#ff8000");
        assert_eq!(d.merged(&json!({"fadeColor": [1, 2, 3]})).unwrap().fade_color, Some([1, 2, 3]));
        // NaN / inf (from a hand-edited file or a caller in-process) take the defaults
        let s = PlaybackSettings { slide_duration: f64::NAN, fade_duration: f64::INFINITY, ..d.clone() }.sanitized();
        assert_eq!((s.slide_duration, s.fade_duration), (d.slide_duration, d.fade_duration));
        // and the timeline never panics on hostile times
        let mut p = Playback::new(ids(2), None, s, 0, f64::NAN).unwrap();
        p.tick(f64::NAN, all_ready);
        p.tick(f64::INFINITY, all_ready);
        p.toggle_pause(f64::NEG_INFINITY);
        p.step(i32::MIN, f64::NAN);
        let _ = p.frame(f64::NAN);
        assert!(Playback::new(vec![], None, d, 0, 0.0).is_none());
    }

    #[test]
    fn a_single_photo_repeats_without_changing() {
        let mut p = Playback::new(ids(1), None, settings(1.0, 0.5), 1, 0.0).unwrap();
        assert_eq!(p.upcoming(2), vec![]);
        assert_eq!(p.tick(5.0, all_ready), Tick::Idle);
        assert_eq!(p.frame(5.0).from, None);
    }
}
