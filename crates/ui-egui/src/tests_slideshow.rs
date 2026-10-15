//! The slideshow in the app (View ▸ Slideshow): playback with the library's settings, slides
//! prepared ahead off the UI thread, transitions only between ready slides, keys and controls.
//! Simulated time: every headless frame is 1/60 s, so nothing here sleeps.

use std::time::Duration;

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::headless::Headless;
use crate::render::Slot;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn grid() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.step();
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn play(h: &Headless) -> &lightcraft_engine::slideshow::Playback {
    &h.app.ui.slideshow.as_ref().expect("a slideshow is playing").play
}

fn has_slide(h: &Headless, id: PhotoId) -> bool {
    h.app.renderer.textures.get(&Slot::Slide(id)).is_some_and(|t| t.photo == id)
}

fn slides_held(h: &Headless) -> usize {
    h.app.renderer.textures.keys().filter(|s| matches!(s, Slot::Slide(_))).count()
}

#[test]
fn slideshow_advances_pauses_and_ends() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0}));
    assert!(h.app.ui.fullscreen && h.app.ui.slideshow.is_some());
    assert_eq!(h.app.session.active(), Some(first), "starts at the first photo in view");
    assert!(h.step_until(SETTLE, |h| h.app.session.active() != Some(first)), "the next photo comes up and is selected");
    // Space pauses: nothing moves
    let r = h.request("ui.key", json!({"key": "space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(play(&h).is_paused(), "paused");
    let held = h.app.session.active();
    for _ in 0..120 {
        h.step();
    }
    assert_eq!(h.app.session.active(), held);
    // Esc ends it, and the slides are let go
    let r = h.request("ui.key", json!({"key": "escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.ui.fullscreen && h.app.ui.slideshow.is_none());
    assert_eq!(slides_held(&h), 0, "slide textures freed");
    h.settle(SETTLE);
}

#[test]
fn next_slides_are_prepared_before_they_are_due() {
    let mut h = grid();
    // a long slide: anything rendered for the next ones is rendered ahead of time
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 1, "preparePreviews": true, "seed": 7}));
    let ahead = play(&h).upcoming(crate::panels::slideshow::AHEAD);
    assert_eq!(ahead.len(), 2);
    let start = h.time();
    // the slide up and the next two; their renders can finish in any order
    let current = play(&h).current();
    assert!(
        h.step_until(SETTLE, |h| has_slide(h, current) && ahead.iter().all(|id| has_slide(h, *id))),
        "the slide up and the next two slides are rendered"
    );
    assert!(h.time() - start < 60.0, "before the first is due");
    assert_eq!(play(&h).current(), current, "still on the first slide");
    h.settle(SETTLE);

    // off: only the slide up is rendered until the next one is due
    let mut h = grid();
    run(&mut h, "view.slideshow", json!({"interval": 60, "preparePreviews": false}));
    let next = play(&h).upcoming(1)[0];
    let current = play(&h).current();
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)));
    for _ in 0..30 {
        h.step();
    }
    assert!(!has_slide(&h, next) && !h.app.renderer.is_pending(Slot::Slide(next)), "not prepared");
    h.settle(SETTLE);
}

#[test]
fn a_fade_only_runs_between_rendered_slides() {
    let mut h = grid();
    // without previews prepared, each slide renders when it is due: the transition must wait
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0.4, "preparePreviews": false}));
    let mut fades = 0;
    let t0 = std::time::Instant::now();
    while fades < 20 && t0.elapsed() < SETTLE {
        h.step();
        let f = play(&h).frame(h.time());
        if let Some(from) = f.from {
            fades += 1;
            assert!(has_slide(&h, f.current), "fading in a slide that isn't rendered");
            assert!(has_slide(&h, from), "fading out a slide that was let go");
        }
    }
    assert!(fades >= 20, "fades ran ({fades})");
    h.settle(SETTLE);
}

#[test]
fn arrows_cut_and_repeat_off_ends_after_the_last_slide() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 2, "repeat": false}));
    let r = h.request("ui.key", json!({"key": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(play(&h).current(), photos[1]);
    assert_eq!(h.app.session.active(), Some(photos[1]), "the selection follows");
    assert!(play(&h).frame(h.time()).from.is_none(), "a cut, not a fade");
    let r = h.request("ui.key", json!({"key": "left"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(play(&h).current(), photos[0]);
    for _ in 1..photos.len() {
        run(&mut h, "slideshow.next", json!({}));
    }
    assert_eq!(play(&h).position(), (photos.len(), photos.len()));
    run(&mut h, "slideshow.next", json!({}));
    assert!(h.app.ui.slideshow.is_none() && !h.app.ui.fullscreen, "no repeat: past the last one it ends");
    h.settle(SETTLE);
}

#[test]
fn random_order_starts_at_the_selection_and_is_fixed_by_the_seed() {
    let order = |seed: u64| {
        let mut h = grid();
        let photos = h.app.session.visible_cloned();
        run(&mut h, "library.select", json!({"ids": [photos[3].0]}));
        run(&mut h, "view.slideshow", json!({"randomOrder": true, "seed": seed, "interval": 60}));
        let mut seen = vec![play(&h).current()];
        for _ in 1..photos.len() {
            run(&mut h, "slideshow.next", json!({}));
            seen.push(play(&h).current());
        }
        h.settle(SETTLE);
        (photos, seen)
    };
    let (photos, a) = order(11);
    assert_eq!(a[0], photos[3]);
    let in_view_order: Vec<PhotoId> = photos.iter().copied().filter(|p| *p != photos[3]).collect();
    assert_ne!(a[1..].to_vec(), in_view_order, "shuffled");
    let mut sorted = a.clone();
    sorted.sort();
    let mut all = photos.clone();
    all.sort();
    assert_eq!(sorted, all, "every photo once");
    assert_eq!(order(11).1, a, "same seed, same order");
}

#[test]
fn playback_controls_show_on_pointer_movement_and_hide() {
    let mut h = grid();
    run(&mut h, "view.slideshow", json!({"interval": 60}));
    h.step();
    let shown = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "icon:slideshowPlayPause");
    assert!(!shown(&h), "hidden until the pointer moves");
    for fx in [0.3, 0.6] {
        let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": fx, "fy": 0.3}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step();
    assert!(shown(&h), "the pointer moved: the controls show");
    assert!(h.painted_text().iter().any(|t| t.contains(&format!("1 / {}", h.app.session.visible_cloned().len()))), "slide n of N");
    let r = h.request("ui.clickWidget", json!({"id": "icon:slideshowPlayPause"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(play(&h).is_paused(), "the play / pause button pauses");
    // the pointer rests away from the bar: gone after a few seconds
    let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": 0.5, "fy": 0.2}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..(60.0 * (crate::panels::slideshow::CONTROLS_SECS + 0.5)) as usize {
        h.step();
    }
    assert!(!shown(&h), "hidden again");
    let r = h.request("ui.hoverWidget", json!({"id": "slideshow:canvas", "fx": 0.55, "fy": 0.25}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "icon:slideshowEnd"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.slideshow.is_none() && !h.app.ui.fullscreen, "the end button ends it");
    h.settle(SETTLE);
}

#[test]
fn playback_settings_drive_the_slideshow_and_bad_ones_are_refused() {
    let mut h = grid();
    let r = run(&mut h, "slideshow.playback", json!({"slideDuration": 9, "fadeDuration": 0.5, "repeat": false}));
    assert_eq!(r["slideDuration"], 9.0);
    let r = h.request("engine.execute", json!({"command": "view.slideshow", "params": {"slideDuration": "slow"}}), T);
    assert_ne!(r["ok"], true, "{r}");
    assert!(h.app.ui.slideshow.is_none());
    let r = h.request("engine.execute", json!({"command": "view.slideshow", "params": {"seed": -1}}), T);
    assert_ne!(r["ok"], true, "{r}");
    let r = run(&mut h, "view.slideshow", json!({}));
    assert_eq!(r["settings"]["slideDuration"], 9.0);
    assert_eq!(r["settings"]["repeat"], false);
    // `interval` (the older parameter) is this run's slide duration; the saved one stays
    run(&mut h, "slideshow.end", json!({}));
    let r = run(&mut h, "view.slideshow", json!({"interval": 2}));
    assert_eq!(r["settings"]["slideDuration"], 2.0);
    assert_eq!(h.app.session.slideshow.slide_duration, 9.0);
    // the options open in Settings
    run(&mut h, "slideshow.end", json!({}));
    run(&mut h, "view.slideshowOptions", json!({}));
    h.step();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "check:settings.slideshowRandom"));
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowRandom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slideshow.random_order, "the checkbox goes through slideshow.playback");
    h.settle(SETTLE);
}

#[test]
fn a_photo_deleted_while_it_plays_does_not_stall_the_slideshow() {
    let mut h = grid();
    let photos = h.app.session.visible_cloned();
    run(&mut h, "view.slideshow", json!({"interval": 0.5, "fadeDuration": 0, "preparePreviews": false}));
    // the next slide goes away before it is due
    let next = play(&h).upcoming(1)[0];
    run(&mut h, "photo.delete", json!({"ids": [next.0]}));
    run(&mut h, "photo.deletePermanently", json!({"ids": [next.0]}));
    assert!(h.app.session.catalog.photo(next).is_none());
    assert!(h.step_until(SETTLE, |h| h.app.ui.slideshow.as_ref().is_some_and(|r| r.play.position().0 >= 3)), "it plays on");
    assert!(photos.len() > 3);
    h.settle(SETTLE);
}

fn painted(img: &egui::ColorImage, x: usize, y: usize) -> [u8; 3] {
    let c = img.pixels[y * img.size[0] + x];
    [c.r(), c.g(), c.b()]
}

#[test]
fn a_composed_slide_shows_during_playback() {
    let mut h = grid();
    let r = run(&mut h, "slideshow.layout", json!({"margins": 0.1, "backdropColor": "#ff0000"}));
    assert_eq!(r["marginTop"], 0.1);
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0}));
    let current = play(&h).current();
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)), "the slide is composed and ready");
    h.step();
    let tex = h.app.renderer.textures.get(&Slot::Slide(current)).unwrap();
    assert_eq!(tex.size, [1000, 700], "a composed slide fills the screen");
    let img = h.paint();
    assert_eq!(img.size, [1000, 700]);
    // 10 % of the 700 px short side = 70 px of red backdrop around the image cell
    for (x, y) in [(5, 5), (995, 5), (500, 30), (500, 690), (30, 350), (985, 350)] {
        assert_eq!(painted(&img, x, y), [255, 0, 0], "margin at ({x}, {y})");
    }
    let mid = painted(&img, 500, 350);
    assert_ne!(mid, [255, 0, 0], "the photo in the cell");
    // the next slide is composed too, and stepping shows it
    let next = play(&h).upcoming(1)[0];
    assert!(h.step_until(SETTLE, |h| has_slide(h, next)));
    run(&mut h, "slideshow.next", json!({}));
    h.step();
    assert_eq!(painted(&h.paint(), 5, 5), [255, 0, 0]);
    h.settle(SETTLE);
}

#[test]
fn the_default_layout_plays_the_photo_as_before() {
    let mut h = grid();
    assert!(h.app.session.slide_layout.is_plain());
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0}));
    let current = play(&h).current();
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)));
    h.step();
    // the photo's own render (its aspect, not the screen's), fitted on black, as before
    let tex = h.app.renderer.textures.get(&Slot::Slide(current)).unwrap();
    let aspect = h.app.session.slide_photo_aspect(current).unwrap();
    let tex_aspect = tex.size[0] as f64 / tex.size[1] as f64;
    assert!((tex_aspect - aspect).abs() < 0.02, "{tex_aspect} vs {aspect}");
    assert_ne!(tex.size, [1000, 700]);
    let img = h.paint();
    let r = crate::panels::detail::fit_texture_rect(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0)), tex.size);
    if r.left() > 2.0 {
        assert_eq!(painted(&img, 1, 350), [0, 0, 0], "black beside the photo");
    }
    if r.top() > 2.0 {
        assert_eq!(painted(&img, 500, 1), [0, 0, 0], "black above the photo");
    }
    h.settle(SETTLE);
}

#[test]
fn settings_shows_the_slide_layout_and_a_live_preview() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    h.step();
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    for w in [
        "slider:slideshowMargins",
        "check:settings.slideshowZoomToFill",
        "check:settings.slideshowStroke",
        "check:settings.slideshowShadow",
        "button:slideshowBackdropColor",
        "check:settings.slideshowWash",
        "slideshow:preview",
    ] {
        assert!(has(&h, w), "{w}");
    }
    // the preview is the composed slide, rendered off the UI thread
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    // a checkbox goes through slideshow.layout, and its controls appear (scrolled into view)
    let r = h.request("ui.hoverWidget", json!({"id": "check:settings.slideshowRandom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.scroll", json!({"dy": -600}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..30 {
        h.step();
    }
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowShadow"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_layout.cast_shadow);
    assert!(has(&h, "slider:slideshowShadowAngle"));
    // a changed layout renders the preview again (once it has settled)
    let before = h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key);
    run(&mut h, "slideshow.layout", json!({"margins": 0.2, "backdropColor": "#00ff00"}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key) != before));
    h.step();
    let rect = h.app.widgets.iter().find(|(id, _)| id == "slideshow:preview").map(|(_, r)| *r).unwrap();
    let img = h.paint();
    let c = painted(&img, (rect.left() + 3.0) as usize, (rect.center().y) as usize);
    assert!(c[1] > 200 && c[0] < 60, "the green backdrop in the preview margin: {c:?}");
    h.settle(SETTLE);
}

#[test]
fn overlays_show_during_playback() {
    let mut h = grid();
    assert!(h.app.session.slides_plain());
    // the default layout with an identity plate: playback composes the slide to draw it
    run(
        &mut h,
        "slideshow.overlays",
        json!({"identityPlate": {"enabled": true, "text": "LC", "size": 0.12, "color": "#00ff00", "opacity": 1, "anchor": "topLeft", "offsetX": 0.02, "offsetY": 0.02}}),
    );
    assert!(!h.app.session.slides_plain());
    run(&mut h, "view.slideshow", json!({"interval": 60, "fadeDuration": 0}));
    let current = play(&h).current();
    assert!(h.step_until(SETTLE, |h| has_slide(h, current)), "the slide is composed and ready");
    h.step();
    assert_eq!(h.app.renderer.textures.get(&Slot::Slide(current)).unwrap().size, [1000, 700]);
    let img = h.paint();
    // the plate's box: 14 px in from the top-left corner, 84 px high
    let green = (14..98).flat_map(|y| (14..120).map(move |x| (x, y))).filter(|&(x, y)| {
        let c = painted(&img, x, y);
        c[1] > 200 && c[0] < 80 && c[2] < 80
    });
    assert!(green.count() > 200, "the identity plate is on the slide");
    h.settle(SETTLE);
}

#[test]
fn settings_shows_the_overlays_and_the_preview_follows_them() {
    let mut h = grid();
    let first = h.app.session.visible_cloned()[0];
    run(&mut h, "library.select", json!({"ids": [first.0]}));
    run(&mut h, "view.slideshowOptions", json!({}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).is_some_and(|t| t.photo == first)));
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    // scroll down to the Overlays section
    let r = h.request("ui.hoverWidget", json!({"id": "check:settings.slideshowRandom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        let r = h.request("ui.scroll", json!({"dy": -800}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..10 {
            h.step();
        }
    }
    for w in ["check:settings.slideshowPlate", "check:settings.slideshowStars", "button:slideshowAddText", "check:settings.slideshowWatermark"] {
        assert!(has(&h, w), "{w}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowAddText"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_overlays.texts.len(), 1);
    h.step();
    assert!(has(&h, "field:slideshowText0"), "the text overlay's template field");
    // its rows are below the fold: scroll on down
    for _ in 0..4 {
        let r = h.request("ui.scroll", json!({"dy": -800}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..10 {
            h.step();
        }
    }
    assert!(has(&h, "button:slideshowText0Anchor-4"), "its anchor grid");
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowText0Anchor-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.slide_overlays.texts[0].place.anchor, lightcraft_engine::export::Anchor::TopLeft, "picked in the grid");
    // removed from its row the text overlay from its row
    let r = h.request("ui.clickWidget", json!({"id": "button:slideshowText0Remove"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_overlays.texts.is_empty());
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.slideshowPlate"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.slide_overlays.identity_plate.enabled);
    // the preview is rendered again with the overlays
    let before = h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key);
    run(&mut h, "slideshow.overlays", json!({"identityPlate": {"text": "Studio", "color": "#ff00ff"}}));
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.get(&Slot::SlidePreview).map(|t| t.key) != before));
    h.settle(SETTLE);
}
