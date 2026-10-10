//! The slideshow (View ▸ Slideshow, ⌥⌘↩): the photos in view, full screen, played with the
//! library's Playback settings (`slideshow.playback`: slide and fade durations, fade colour,
//! random order, repeat, preparing previews in advance).
//!
//! The timeline is [`lightcraft_engine::slideshow::Playback`] (no egui, tested on its own); this
//! module renders the slides, draws the frame it asks for and the playback controls.
//!
//! Smooth transitions: every slide is rendered into its own texture ([`Slot::Slide`]) off the UI
//! thread. With "prepare previews" on, the next [`AHEAD`] slides are requested as soon as a slide
//! comes up, so they are ready long before they are due; a fade then only draws two textures
//! that already exist (no render, no upload inside the fade). The timeline does not start a
//! transition until the next slide's texture exists ([`ready`]): the current slide is held
//! instead of flashing a stand-in. With it off, a slide is rendered when it is due.
//!
//! Which picture a slide shows comes from one place, [`slide_job`], so a later slide composer
//! (layout, overlays, backdrop) replaces that function and the playback stays as it is.

use egui::{Align2, Color32, Pos2, Rect, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::slideshow::{Playback, Tick};
use lightcraft_pipeline::geometry::Frame;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::Icon;
use crate::render::Slot;
use crate::widgets::{icon_button, register};

/// Slides prepared ahead of the one up.
pub const AHEAD: usize = 2;
/// Seconds the playback controls stay after the pointer last moved.
pub const CONTROLS_SECS: f64 = 2.5;

/// A running slideshow.
#[derive(Clone, Debug)]
pub struct Run {
    pub play: Playback,
    /// The playback controls show until this time (egui time).
    pub controls_until: f64,
    /// Where the pointer was last frame (the controls appear when it moves).
    pub pointer: Option<Pos2>,
    /// Slides no render can be made for (gone from the catalog, nothing to read): shown with
    /// whatever stand-in there is, never waited for.
    pub unrenderable: Vec<PhotoId>,
}

/// `view.slideshow {interval?, seed?, slideDuration?, fadeDuration?, fadeColor?, randomOrder?,
/// repeat?, preparePreviews?}`: play the photos in view from the selected one with the library's
/// playback settings; the keys given change them for this run only (`interval` = slideDuration,
/// `seed` fixes the random order).
pub fn start(app: &mut LightcraftApp, p: &Value, now: f64) -> Result<Value, String> {
    let mut overrides = p.as_object().cloned().unwrap_or_default();
    if let Some(i) = overrides.remove("interval") {
        overrides.insert("slideDuration".into(), i);
    }
    let seed = match overrides.remove("seed") {
        None | Some(Value::Null) => (now.abs() * 1000.0) as u64 ^ 0x5eed_5eed,
        Some(v) => v.as_u64().ok_or_else(|| format!("view.slideshow: `seed` must be a whole number, not {v}"))?,
    };
    let settings = app.session.slideshow.merged(&Value::Object(overrides)).map_err(|e| format!("view.slideshow: {e}"))?;
    let photos = app.session.visible_cloned();
    let Some(play) = Playback::new(photos, app.session.active(), settings, seed, now) else {
        return Err("no photos to show".into());
    };
    let first = play.current();
    if app.session.active() != Some(first) {
        let _ = app.run("library.select", json!({"ids": [first.0]}));
    }
    let (_, slides) = play.position();
    let out = json!({"slides": slides, "seed": seed, "settings": play.settings().to_json()});
    app.ui.slideshow = Some(Box::new(Run { play, controls_until: 0.0, pointer: None, unrenderable: Vec::new() }));
    app.ui.fullscreen = true;
    app.ui.zoom = crate::state::Zoom::Fit;
    app.ui.tool.clear();
    let _ = app.session.end_interaction();
    Ok(out)
}

/// End the slideshow (back to the view it started from, on the last photo shown).
pub fn end(app: &mut LightcraftApp) {
    app.ui.slideshow = None;
    app.ui.fullscreen = false;
    app.renderer.release_slides(&[]);
}

/// The slideshow's own commands (the playback controls and their keys).
pub fn command(app: &mut LightcraftApp, id: &str, now: f64) -> Result<Value, String> {
    let Some(run) = app.ui.slideshow.as_mut() else { return Err("no slideshow is playing".into()) };
    let t = match id {
        "slideshow.togglePause" => {
            run.play.toggle_pause(now);
            return Ok(state(&run.play, now));
        }
        "slideshow.next" => run.play.step(1, now),
        "slideshow.previous" => run.play.step(-1, now),
        _ => {
            end(app);
            return Ok(Value::Null);
        }
    };
    after(app, t);
    Ok(app.ui.slideshow.as_ref().map(|r| state(&r.play, now)).unwrap_or(json!({"ended": true})))
}

/// What the slideshow is doing (for `slideshow.*` replies and tests).
pub fn state(play: &Playback, now: f64) -> Value {
    let (n, of) = play.position();
    let f = play.frame(now);
    json!({
        "current": play.current().0,
        "slide": n,
        "of": of,
        "paused": play.is_paused(),
        "fading": f.from.is_some(),
        "progress": f.progress,
        "waitingFor": play.waiting_for().map(|p| p.0),
        "remaining": play.remaining(now),
    })
}

/// Act on what the timeline did: follow the slide with the selection, or end.
fn after(app: &mut LightcraftApp, t: Tick) {
    match t {
        Tick::Advanced(id) if app.session.active() != Some(id) => {
            let _ = app.run("library.select", json!({"ids": [id.0]}));
        }
        Tick::Ended => end(app),
        _ => {}
    }
}

/// Can `id` be shown as a slide now: its texture exists (or it can't be rendered at all, and its
/// stand-in is all there will be: playback goes on rather than waiting forever).
pub fn ready(app: &LightcraftApp, id: PhotoId) -> bool {
    app.renderer.textures.get(&Slot::Slide(id)).is_some_and(|t| t.photo == id)
        || app.renderer.failure(Slot::Slide(id)).is_some()
        || app.session.catalog.photo(id).is_none()
        || app.ui.slideshow.as_ref().is_some_and(|r| r.unrenderable.contains(&id))
}

/// Advance a running slideshow (every frame, before drawing).
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(run) = app.ui.slideshow.as_ref() else {
        app.renderer.release_slides(&[]);
        return;
    };
    if !app.ui.fullscreen {
        end(app);
        return;
    }
    let now = ctx.input(|i| i.time);
    let mut play = run.play.clone();
    let t = play.tick(now, |id| ready(app, id));
    if let Some(run) = app.ui.slideshow.as_mut() {
        run.play = play;
    }
    after(app, t);
    let Some(run) = app.ui.slideshow.as_ref() else { return };
    let f = run.play.frame(now);
    if f.from.is_some() {
        ctx.request_repaint(); // a fade draws every frame
    } else if !run.play.is_paused() {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(run.play.remaining(now).clamp(0.016, 120.0)));
    }
    if run.controls_until > now {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((run.controls_until - now).clamp(0.016, CONTROLS_SECS)));
    }
}

/// The render that shows slide `id` in `area` (logical points at `ppp`): the photo as developed,
/// fitted to the screen. The one place that decides what a slide's picture is.
fn slide_job(app: &mut LightcraftApp, id: PhotoId, area: Rect, ppp: f32, texture_side: usize) -> Option<lightcraft_engine::RenderJob> {
    let photo = app.session.catalog.photo(id)?.clone();
    let frame = Frame::with_lens(photo.width.max(1) as usize, photo.height.max(1) as usize, &photo.develop, true, photo.embedded_lens.as_ref());
    let aspect = frame.aspect() as f32;
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.5 };
    let fit = (area.width() / aspect).min(area.height());
    let drawn_long = fit.max(fit * aspect) * ppp;
    let native = super::detail::output_px(&frame);
    let edge = app.ui.settings.loupe_edge(drawn_long, native[0].max(native[1]), texture_side);
    let (w, h) = if aspect >= 1.0 { (edge, (edge as f32 / aspect) as usize) } else { ((edge as f32 * aspect) as usize, edge) };
    app.session.loupe_job(id, w.max(8), h.max(8), true)
}

/// The slides to keep rendered now, most wanted first, with their render priority.
fn wanted(play: &Playback, now: f64) -> Vec<(PhotoId, u32)> {
    let f = play.frame(now);
    let mut v = vec![(f.current, 100)];
    if let Some(from) = f.from {
        v.push((from, 99));
    }
    if let Some(next) = play.waiting_for() {
        v.push((next, 98));
    }
    if play.settings().prepare_previews {
        for (k, id) in play.upcoming(AHEAD).into_iter().enumerate() {
            v.push((id, 97u32.saturating_sub(k as u32)));
        }
    }
    let mut seen = Vec::new();
    v.retain(|(id, _)| {
        let fresh = !seen.contains(id);
        seen.push(*id);
        fresh
    });
    v
}

/// The texture to draw for slide `id`: its own render, else the loupe's or a thumbnail (a slide
/// stepped to with ←/→ shows at once, sharp when its render lands).
fn texture(app: &LightcraftApp, id: PhotoId) -> Option<&crate::render::Tex> {
    app.renderer
        .textures
        .get(&Slot::Slide(id))
        .filter(|t| t.photo == id)
        .or_else(|| app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id))
        .or_else(|| app.renderer.thumb(id))
}

fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

/// The parts of `outer` outside `inner` (up to four bands).
fn outside(outer: Rect, inner: Rect) -> Vec<Rect> {
    let i = inner.intersect(outer);
    if !i.is_positive() {
        return vec![outer];
    }
    let mut v = Vec::new();
    if i.top() > outer.top() {
        v.push(Rect::from_min_max(outer.min, pos2(outer.right(), i.top())));
    }
    if i.bottom() < outer.bottom() {
        v.push(Rect::from_min_max(pos2(outer.left(), i.bottom()), outer.max));
    }
    if i.left() > outer.left() {
        v.push(Rect::from_min_max(pos2(outer.left(), i.top()), pos2(i.left(), i.bottom())));
    }
    if i.right() < outer.right() {
        v.push(Rect::from_min_max(pos2(i.right(), i.top()), pos2(outer.right(), i.bottom())));
    }
    v
}

/// Draw the slideshow (full screen) and its controls.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let canvas = ui.max_rect();
    app.canvas_rect = Some(canvas);
    // (`ui.pointer` positions are relative to it)
    app.image_rect = Some(canvas);
    let Some(run) = app.ui.slideshow.as_ref() else { return };
    let now = ui.input(|i| i.time);
    let ppp = ui.ctx().pixels_per_point();
    let side = super::detail::texture_side(ui.ctx());
    let want = wanted(&run.play, now);
    let frame = run.play.frame(now);
    let mut unrenderable = Vec::new();
    for (id, priority) in &want {
        match slide_job(app, *id, canvas, ppp, side) {
            Some(job) => app.renderer.request(Slot::Slide(*id), job, *priority),
            None => unrenderable.push(*id),
        }
    }
    if let Some(run) = app.ui.slideshow.as_mut() {
        for id in unrenderable {
            if !run.unrenderable.contains(&id) {
                run.unrenderable.push(id);
            }
        }
    }
    let keep: Vec<PhotoId> = want.iter().map(|(id, _)| *id).collect();
    app.renderer.release_slides(&keep);
    let p = ui.painter_at(canvas);
    p.rect_filled(canvas, 0.0, Color32::BLACK);
    let rect_of = |t: &crate::render::Tex| super::detail::fit_texture_rect(canvas, t.size);
    let draw = |id: PhotoId, alpha: f32| -> Option<Rect> {
        let t = texture(app, id)?;
        let r = rect_of(t);
        p.image(t.tex.id(), r, full_uv(), Color32::WHITE.gamma_multiply(alpha.clamp(0.0, 1.0)));
        Some(r)
    };
    let k = frame.progress.clamp(0.0, 1.0);
    match (frame.from, frame.fade_color) {
        // fade through a colour: out to it, then in from it
        (Some(from), Some(c)) => {
            let veil = if k < 0.5 {
                draw(from, 1.0);
                k * 2.0
            } else {
                draw(frame.current, 1.0);
                2.0 - k * 2.0
            };
            p.rect_filled(canvas, 0.0, Color32::from_rgb(c[0], c[1], c[2]).gamma_multiply(veil.clamp(0.0, 1.0)));
        }
        // crossfade: where the slides overlap the new one goes over the old at `k`; where only
        // the old one is, it fades to the backdrop
        (Some(from), None) => {
            let new_rect = texture(app, frame.current).map(rect_of);
            if let Some(t) = texture(app, from) {
                let r = rect_of(t);
                match new_rect {
                    Some(n) => {
                        p.with_clip_rect(n.intersect(canvas)).image(t.tex.id(), r, full_uv(), Color32::WHITE);
                        for band in outside(r, n) {
                            p.with_clip_rect(band).image(t.tex.id(), r, full_uv(), Color32::WHITE.gamma_multiply(1.0 - k));
                        }
                    }
                    None => {
                        p.image(t.tex.id(), r, full_uv(), Color32::WHITE.gamma_multiply(1.0 - k));
                    }
                }
            }
            draw(frame.current, k);
        }
        _ => {
            draw(frame.current, 1.0);
        }
    }
    register(ui.ctx(), "slideshow:canvas", canvas);
    controls(app, ui, canvas, now);
}

/// The playback controls: previous, play / pause, next, slide n of N, end. They appear when the
/// pointer moves and fade out [`CONTROLS_SECS`] after it stops (or stay while it is over them).
fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, now: f64) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let pos = ui.input(|i| i.pointer.latest_pos());
    let pressed = ui.input(|i| i.pointer.any_pressed());
    let bar = Rect::from_center_size(pos2(canvas.center().x, canvas.bottom() - 56.0), vec2(300.0, 44.0));
    let Some(run) = app.ui.slideshow.as_mut() else { return };
    let moved = pos.is_some() && run.pointer.is_some() && pos != run.pointer;
    let over = pos.is_some_and(|p| bar.expand(8.0).contains(p));
    if moved || pressed || (over && run.controls_until > now) {
        run.controls_until = now + CONTROLS_SECS;
    }
    run.pointer = pos.or(run.pointer);
    let left = run.controls_until - now;
    if left <= 0.0 {
        return;
    }
    let paused = run.play.is_paused();
    let (n, of) = run.play.position();
    // fade out over the last half second
    let alpha = (left / 0.5).clamp(0.0, 1.0) as f32;
    let p = ui.painter_at(canvas);
    p.rect_filled(bar, 10.0, Color32::from_black_alpha((200.0 * alpha) as u8));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.set_opacity(alpha);
    child.spacing_mut().item_spacing.x = 4.0;
    let size = vec2(36.0, 36.0);
    let mut fire = None;
    if icon_button(&mut child, "slideshowPrevious", Icon::ChevronLeft, size, false, true, "Previous slide (←)").clicked() {
        fire = Some("slideshow.previous");
    }
    let (icon, tip) = if paused { (Icon::Play, "Play (Space)") } else { (Icon::Pause, "Pause (Space)") };
    if icon_button(&mut child, "slideshowPlayPause", icon, size, false, true, tip).clicked() {
        fire = Some("slideshow.togglePause");
    }
    if icon_button(&mut child, "slideshowNext", Icon::ChevronRight, size, false, true, "Next slide (→)").clicked() {
        fire = Some("slideshow.next");
    }
    let (r, _) = child.allocate_exact_size(vec2(118.0, 36.0), egui::Sense::hover());
    let label = format!("{n} / {of}");
    child.painter().text(r.center(), Align2::CENTER_CENTER, label, t.font(13.0), Color32::from_gray(230).gamma_multiply(alpha));
    if icon_button(&mut child, "slideshowEnd", Icon::Close, size, false, true, "End slideshow (Esc)").clicked() {
        fire = Some("slideshow.end");
    }
    if let Some(id) = fire {
        let _ = app.run(id, json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossfade_bands_cover_the_old_slide_outside_the_new_one() {
        let outer = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 60.0));
        let inner = Rect::from_min_max(pos2(30.0, 0.0), pos2(70.0, 60.0));
        let bands = outside(outer, inner);
        let area: f32 = bands.iter().map(|b| b.area()).sum();
        assert_eq!(area, 100.0 * 60.0 - 40.0 * 60.0);
        assert_eq!(outside(outer, Rect::from_min_max(pos2(200.0, 0.0), pos2(300.0, 10.0))), vec![outer], "no overlap: all of it");
        assert!(outside(outer, outer.expand(5.0)).is_empty(), "covered");
    }
}
