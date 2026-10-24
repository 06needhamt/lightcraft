//! 0.37 lifecycle regressions: the app clock, one-consumer queues, one pending synthetic batch,
//! parent-salted panel state, bundled monochrome emoji, and bounded/stale-safe screenshot capture.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use crate::headless::Headless;
use crate::render::{Slot, Tex};
use crate::{ControlResponse, LightcraftApp, MAX_PENDING_SHOTS, MAX_SHOT_BYTES, PendingShot, Services, ShotGeometry};
use lightcraft_engine::slideshow::Item;

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

fn raw(size: egui::Vec2) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() }
}

fn viewport_raw(size: egui::Vec2, visible: bool) -> egui::RawInput {
    let mut raw = raw(size);
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(!visible);
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().occluded = Some(false);
    raw
}

fn reply_channel() -> (Sender<ControlResponse>, Receiver<ControlResponse>) {
    channel()
}

fn geometry(width: f32, height: f32) -> ShotGeometry {
    ShotGeometry { viewport: egui::vec2(width, height), pixels_per_point: 1.0, zoom: 1.0, epoch: 0 }
}

fn shot_bytes_for(width: usize, height: usize) -> usize {
    width * height * std::mem::size_of::<egui::Color32>()
}

/// One layout pass whose texture delta is consumed. 0.37's `TexturesDelta` panics when a non-empty
/// delta is dropped, so a test that only inspects input/state must still acknowledge the font atlas
/// the pass produced. `HeadlessView::run` does this with `TexturesDelta::apply`; a bare
/// `Context::run_ui` has no renderer, so clearing the delta is the explicit handling.
fn frame(ctx: &egui::Context, raw: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) {
    let mut output = ctx.run_ui(raw, ui);
    output.textures_delta.clear();
}

/// An app whose screenshot encoder writes through the real file path and records the pixels it saw.
fn app_with_screenshot_files() -> (LightcraftApp, Arc<Mutex<Option<(usize, usize, Vec<u8>)>>>) {
    let mut app = app();
    let seen = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&seen);
    app.services.png = Some(Box::new(move |img: &lightcraft_raster::Rgba8| {
        *recorder.lock().unwrap() = Some((img.width, img.height, img.as_bytes()));
        vec![0x89, b'P', b'N', b'G']
    }));
    app.services.write = Some(Box::new(|path: &str, bytes: &[u8]| std::fs::write(path, bytes).map_err(|e| e.to_string())));
    app.services.write_new = Some(Box::new(|path: &str, bytes: &[u8]| lightcraft_engine::export::write_file_new(path, bytes)));
    (app, seen)
}

fn temp_png(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("lightcraft-{name}-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Bind a pending request to the geometry/library generation a real issue would have used.
fn issue_into_slot(ctx: &egui::Context, shot: &mut PendingShot, issued: ShotGeometry, library: u64) -> egui::ScreenshotCallback {
    let (slot, callback) = craft_ui::capture::CaptureSlot::new(ctx, egui::ViewportId::ROOT, MAX_SHOT_BYTES);
    shot.slot = Some(slot);
    shot.sent_at_ms = Some(0.0);
    shot.issued = Some(issued);
    shot.issued_library = library;
    callback
}

#[test]
fn bundled_faces_include_monochrome_emoji() {
    // 0.37 moved the monochrome emoji faces behind `monochrome_emoji_fonts`, and `default_fonts`
    // is no longer an egui default. Both come from the root `egui` features (kittest's own
    // dev-dependencies do not propagate), so this test fails if either is lost.
    let definitions = crate::theme::font_definitions(&[]);
    for name in ["Hack", "Ubuntu-Light", "egui-icons", "NotoEmoji-Regular", "emoji-icon-font"] {
        assert!(definitions.font_data.contains_key(name), "bundled face {name} is missing: {:?}", definitions.font_data.keys().collect::<Vec<_>>());
    }
    let ctx = egui::Context::default();
    ctx.set_fonts(definitions);
    // `Fonts` needs a first pass for its pixels-per-point before `fonts_mut` may inspect glyphs.
    frame(&ctx, raw(egui::vec2(240.0, 120.0)), |ui| {
        // U+1F600 and U+1F44D are in the bundled font cmaps. The older bundled subset
        // does not contain U+1F642, so it cannot serve as a feature-wiring oracle.
        ui.label("😀👍");
    });
    let font = egui::FontId::proportional(16.0);
    assert!(ctx.fonts_mut(|f| f.has_glyph(&font, '\u{1F600}')), "😀 must be paintable from the bundled faces");
    assert!(ctx.fonts_mut(|f| f.has_glyph(&font, '\u{1F44D}')), "👍 must be paintable from the bundled faces");
}

#[test]
fn resizable_side_stores_panel_state_under_the_parent_salted_key() {
    let ctx = egui::Context::default();
    let raw = raw(egui::vec2(1000.0, 700.0));
    frame(&ctx, raw, |ui| {
        crate::panels::resizable_side(ui, true, "side-test", egui::Frame::NONE, 260.0, crate::state::LEFT_WIDTH, 300.0, |ui| {
            ui.label("side");
        });
    });
    let modern = ctx.viewport_id().root_ui_id().with("side-test");
    assert!(
        egui::containers::panel::PanelState::load(&ctx, modern).is_some(),
        "0.37 stores panel state under parent_ui.scope_id().with_salt(id_salt)"
    );
    assert!(
        egui::containers::panel::PanelState::load(&ctx, egui::Id::unique("side-test")).is_none(),
        "an already-built Id must not be salted a second time (the 0.36 exact-Id key)"
    );
}

#[test]
fn legacy_panel_state_is_dropped_once_and_modern_keys_survive() {
    let (ctx, mut app) = (egui::Context::default(), app());
    let state = || egui::containers::panel::PanelState { outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)) };
    let modern = ctx.viewport_id().root_ui_id();
    let salts = ["top_bar", "bottom_bar", "tool_strip", "library-problem-banner"];
    for salt in salts {
        let legacy = egui::Id::unique(salt);
        ctx.data_mut(|d| d.insert_persisted(legacy, state()));
        ctx.data_mut(|d| d.insert_persisted(modern.with(salt), state()));
    }
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |_| {});
    app.logic(&ctx);
    for salt in salts {
        let legacy = egui::Id::unique(salt);
        assert!(egui::containers::panel::PanelState::load(&ctx, legacy).is_none(), "the dead 0.36 key for {salt} is removed");
        assert!(egui::containers::panel::PanelState::load(&ctx, modern.with(salt)).is_some(), "the 0.37 parent-salted key for {salt} is not touched");
    }
}

#[test]
fn capture_admission_bounds_count_and_bytes_before_issue() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    for _ in 0..MAX_PENDING_SHOTS {
        let (tx, _rx) = reply_channel();
        app.admit_shot(&ctx, None, false, tx).expect("admission within all limits");
    }
    let per_image = shot_bytes_for(100, 100);
    assert_eq!(app.shot_bytes, MAX_PENDING_SHOTS * per_image);
    let (tx, _rx) = reply_channel();
    assert!(app.admit_shot(&ctx, None, false, tx).is_err(), "a fifth request is refused before issue");
    // The count gate runs before geometry admission. Free the four slots so the next check reaches
    // the byte branch instead of answering the count error again.
    app.pending_screenshots.clear();
    app.shot_bytes = 0;

    // An image over the per-image cap is refused, and nothing is reserved for it. The geometry is
    // inside the texture limit (16 384 × 2 049), so this exercises the byte branch, not `pixels()`.
    let before = app.shot_bytes;
    app.last_shot_geometry = Some(geometry(16_384.0, 2_049.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("128 MiB + one row is over the per-image cap");
    assert!(error.contains("134283264"), "the error names the image size: {error}");
    assert!(error.contains("134217728"), "the error names the per-image cap: {error}");
    assert_eq!(app.shot_bytes, before, "a refused request reserves no bytes");

    // Two 100 MB surfaces fit the 128 MiB per-image cap, but a third would exceed the 256 MiB
    // aggregate budget (completed-but-undrained images stay counted).
    let (ctx, mut app) = (egui::Context::default(), self::app());
    app.last_shot_geometry = Some(geometry(5000.0, 5000.0));
    for _ in 0..2 {
        let (tx, _rx) = reply_channel();
        app.admit_shot(&ctx, None, false, tx).expect("100 MB is under the per-image cap");
    }
    let (tx, _rx) = reply_channel();
    assert!(app.admit_shot(&ctx, None, false, tx).is_err(), "the aggregate budget bounds a third image");
    assert_eq!(app.shot_bytes, 200_000_000);
}

#[test]
fn texture_limit_and_missing_surface_have_distinct_admission_errors() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.max_texture_side = 1024;
    app.last_shot_geometry = Some(geometry(1200.0, 100.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("wider than the host's texture limit");
    assert!(error.contains("1200×100") && error.contains("1024"), "the error names the surface and the host limit: {error}");
    assert_eq!(app.shot_bytes, 0);

    app.last_shot_geometry = Some(geometry(0.0, 0.0));
    let (tx, _rx) = reply_channel();
    let error = app.admit_shot(&ctx, None, false, tx).expect_err("no laid-out surface");
    assert!(error.contains("no usable surface size"), "the error names the missing layout, not the texture limit: {error}");
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn stale_geometry_completion_is_rejected_before_any_write() {
    let (ctx, mut app) = (egui::Context::default(), app());
    let path = temp_png("stale-shot");
    let (tx, rx) = reply_channel();
    let issued = ShotGeometry { epoch: 7, ..geometry(100.0, 100.0) };
    app.last_shot_geometry = Some(issued);
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let library = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, ShotGeometry { epoch: 7, ..geometry(100.0, 100.0) }, library)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    // The window changed size while the callback was in flight: the image belongs to a stale
    // generation and must be retried, never written.
    app.last_shot_geometry = Some(ShotGeometry { epoch: 8, ..geometry(100.0, 100.0) });
    app.collect_screenshots(&ctx);
    assert_eq!(app.pending_screenshots.len(), 1, "the request is retried");
    assert!(app.pending_screenshots[0].slot.is_none(), "the stale slot is dropped");
    assert!(!path.exists(), "a stale image is never written");
    assert!(rx.try_recv().is_err(), "a retry replies only when it succeeds or times out");
    assert_eq!(app.shot_bytes, shot_bytes_for(100, 100), "the reservation is kept for the retry");
}

#[test]
fn a_completed_capture_writes_once_and_releases_its_reservation() {
    let (ctx, mut app, seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("happy-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::from_rgb(1, 2, 3))));
    app.collect_screenshots(&ctx);

    let reply = rx.try_recv().expect("exactly one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["width"], 2);
    assert_eq!(reply["result"]["height"], 2);
    assert_eq!(std::fs::read(&path).expect("the file is written"), vec![0x89, b'P', b'N', b'G']);
    let seen = seen.lock().unwrap().clone().expect("the encoder ran");
    assert_eq!((seen.0, seen.1), (2, 2));
    assert_eq!(seen.2, vec![1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255]);
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "success returns the reservation");
    assert!(rx.try_recv().is_err(), "one reply, not two");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_capture_completed_after_a_catalog_edit_is_still_written() {
    // Catalog edits bump `catalog.revision` on every op; that is content, not document identity.
    // Binding the capture to it retried once per edit and could only end in a timeout.
    let (ctx, mut app, _seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("edited-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let generation = app.shot_document_generation();
    let issued = geometry(100.0, 100.0);
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.session.catalog.revision = app.session.catalog.revision.wrapping_add(1);
    assert_eq!(app.shot_document_generation(), generation, "an edit is not a new library");
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(path.exists());
    assert_eq!(app.shot_bytes, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_image_larger_than_its_reservation_is_grown_and_written() {
    // Wayland has no `inner_rect`, so on a hidden tick the reservation can be based on the old
    // content rect while the compositor returns the resized surface. A legal image is not an error.
    let (ctx, mut app, _seen) = {
        let (app, seen) = app_with_screenshot_files();
        (egui::Context::default(), app, seen)
    };
    let path = temp_png("grown-shot");
    let (tx, rx) = reply_channel();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("pin the path");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(egui::ColorImage::filled([200, 200], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["width"], 200);
    assert_eq!(app.shot_bytes, 0, "the grown reservation is released too");
    assert!(path.exists());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_surface_that_grew_past_the_per_image_cap_is_refused_by_issue() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(0.0));
    app.ui_passes = 3;
    app.fonts_ready = true;
    app.last_shot_geometry = Some(geometry(5000.0, 5000.0));
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("100 MB is admitted");
    app.set_simulated_clock(Some(1.0));
    app.ui_passes = 6;
    // The window grew after admission: 7000² × 4 = 196 MB, over the 128 MiB per-image cap.
    app.last_shot_geometry = Some(geometry(7000.0, 7000.0));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the refusal is a reply");
    assert_eq!(reply["ok"], false, "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("196000000"), "the error names the new image, not the delta: {error}");
    assert!(error.contains("134217728"), "{error}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn timeout_replies_once_and_a_late_callback_is_inert() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("admitted");
    let issued = geometry(100.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.set_simulated_clock(Some(10.0));
    app.collect_screenshots(&ctx);
    let reply = rx.try_recv().expect("the timeout reply");
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap_or_default().contains("timed out"), "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "a timed-out request returns its reservation");
    // The renderer may still deliver the cancelled one-shot: it finds no slot and changes nothing.
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "no second reply");
    assert_eq!(app.shot_bytes, 0);
}

#[test]
fn a_busy_renderer_cannot_turn_a_settled_capture_into_a_timeout() {
    let (tx, _rx) = reply_channel();
    let shot = PendingShot {
        token: 1,
        target: None,
        reply: tx,
        headless: false,
        admitted_passes: 3,
        not_before_ms: 120.0,
        force_ms: 3000.0,
        deadline_ms: 5120.0,
        sent_at_ms: None,
        reserved: 1,
        slot: None,
        issued: None,
        issued_library: 0,
    };
    assert!(!crate::shot_ready(100.0, &shot, false, 6), "settle time has not passed");
    assert!(crate::shot_ready(200.0, &shot, false, 6), "an idle renderer captures as soon as it is settled");
    assert!(!crate::shot_ready(200.0, &shot, true, 6), "a busy renderer still waits for the settle window");
    assert!(crate::shot_ready(3000.0, &shot, true, 6), "the forced instant captures the current frame anyway");
}

#[test]
fn a_silent_compositor_falls_back_to_the_headless_capture_once() {
    let (ctx, mut app) = (egui::Context::default(), app());
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(160.0, 100.0));
    app.ui_passes = 6;
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, false, tx).expect("a windowed request is admitted");
    let issued = geometry(160.0, 100.0);
    let generation = app.shot_document_generation();
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(&ctx, shot, issued, generation)
    };
    app.ui_passes = 9;
    // Two seconds after the command went out, no compositor frame has arrived.
    app.set_simulated_clock(Some(2.5));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the fallback answers");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0);
    // The canceled compositor callback may still fire; it must not produce a second reply.
    callback.complete(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE)));
    app.collect_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "one reply from the fallback");
}

#[test]
fn a_busy_renderer_still_answers_a_headless_capture_at_the_forced_instant() {
    let (ctx, mut app) = (egui::Context::default(), app());
    // Install fonts and reach the "ready" state a real host has after its setup frame.
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready, "fonts are installed for the shadow frame");
    app.set_simulated_clock(Some(0.0));
    app.last_shot_geometry = Some(geometry(160.0, 100.0));
    app.ui_passes = 10;
    let (tx, rx) = reply_channel();
    app.admit_shot(&ctx, None, true, tx).expect("a headless request is admitted");
    app.ui_passes = 13;
    app.renderer.force_in_flight_for_test(1);
    app.set_simulated_clock(Some(1.0));
    app.issue_screenshots(&ctx);
    assert!(rx.try_recv().is_err(), "a busy renderer defers the capture before the forced instant");
    assert_eq!(app.pending_screenshots.len(), 1);
    app.set_simulated_clock(Some(4.0));
    app.issue_screenshots(&ctx);
    let reply = rx.try_recv().expect("the forced instant answers, it does not time out");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "the grown/shrunk reservation is released");
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct TestDropped(std::path::PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl egui::DroppedFile for TestDropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(vec![])
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn one_synthetic_batch_until_a_completed_pass_and_drops_wait_for_the_ui_phase() {
    let mut app = app();
    app.fonts_ready = true;
    app.synthetic.push(egui::Event::PointerMoved(egui::pos2(4.0, 4.0)));
    let mut first = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&mut first);
    assert_eq!(first.events.len(), 1, "one pointer move is injected");
    assert!(app.synthetic_in_flight);
    let mut second = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&mut second);
    assert!(second.events.is_empty(), "no second step while the first waits for a UI pass");

    // Drops are queued by the hook even while a batch is in flight, but only the UI phase takes them.
    second.dropped_files.push(Arc::new(TestDropped(std::env::temp_dir().join("lightcraft-drop.txt"))));
    app.raw_input_hook(&mut second);
    assert_eq!(app.dropped_files.len(), 1);
    let ctx = egui::Context::default();
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |_| {});
    app.logic(&ctx);
    app.logic(&ctx);
    assert_eq!(app.dropped_files.len(), 1, "a hidden logic tick never replays the drop");
    assert_eq!(app.take_dropped_files().len(), 1, "the UI phase takes the queued drop exactly once");
    assert!(app.dropped_files.is_empty());

    // A completed, nondiscarded app pass (not a bare `run_ui` closure) ends the in-flight batch, so
    // the next host frame injects the next synthetic step. This fails if `finish_ui_pass` stops
    // clearing the gate.
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| app.ui(ui));
    assert!(!app.synthetic_in_flight, "a completed UI pass consumed the batch");
    app.synthetic.push(egui::Event::PointerMoved(egui::pos2(9.0, 9.0)));
    let mut third = raw(egui::vec2(800.0, 600.0));
    app.raw_input_hook(&mut third);
    assert_eq!(third.events.len(), 1, "the next batch is injected after the completed pass");
}

#[test]
fn first_headless_frame_runs_logic_and_uses_its_simulated_clock() {
    let mut h = Headless::new(app(), [800.0, 600.0], 1.0);
    assert_eq!(h.time(), 0.0);
    h.step();
    assert!(h.app.styled, "the first frame at t=0 runs logic and installs fonts");
    assert_eq!(h.app.tick_now(), 0.0, "the app clock follows the headless host's simulated time");
    h.step();
    h.step();
    assert!(h.app.fonts_ready, "fonts are ready after the setup pass");
    assert_eq!(h.app.ui_passes, 2, "exactly the two steps after the setup pass were laid out");
    assert!(h.app.tick_now() > 0.0, "the simulated clock advanced with the steps");
}

#[test]
fn import_ticks_once_per_host_frame_and_once_per_hidden_tick() {
    let ctx = egui::Context::default();
    let seen = std::cell::RefCell::new(Vec::new());
    frame(&ctx, raw(egui::vec2(800.0, 600.0)), |ui| {
        seen.borrow_mut().push(crate::import_tick_allowed(ui.ctx()));
        if seen.borrow().len() == 1 {
            ui.ctx().request_discard("test: force a second layout pass");
        }
    });
    assert_eq!(*seen.borrow(), vec![true, false], "the first pass commits import batches; the discarded re-run does not");

    let hidden = viewport_raw(egui::vec2(800.0, 600.0), false);
    let mut allowed = false;
    let _ = ctx.run_logic(&hidden, |ctx| allowed = crate::import_tick_allowed(ctx));
    assert!(allowed, "a hidden logic tick reports pass index 0 and must still commit import batches");
}

#[test]
fn hidden_slideshow_pauses_and_keeps_its_remaining_time() {
    let (ctx, mut app) = (egui::Context::default(), app());
    app.set_simulated_clock(Some(10.0));
    let started = crate::panels::slideshow::start(
        &mut app,
        &serde_json::json!({"slideDuration": 2.0, "fadeDuration": 0.0, "randomOrder": false, "repeat": false}),
        10.0,
    );
    assert!(started.is_ok(), "the demo library has slides: {started:?}");
    // Every slide is "ready": without the hidden gate, 10 hidden seconds would advance the show.
    for id in app.session.visible_cloned() {
        let tex = ctx.load_texture(format!("slide-{}", id.0), egui::ColorImage::filled([2, 2], egui::Color32::WHITE), Default::default());
        app.renderer
            .textures
            .insert(Slot::Slide(Item::Photo(id)), Tex { key: 0, photo: id, tex, size: [2, 2], histogram: None, ms: 0.0, quick: None, pixels: None });
    }
    let first = app.ui.slideshow.as_ref().unwrap().play.current();
    let selection = app.session.active();
    app.set_simulated_clock(Some(10.5));
    let remaining = app.ui.slideshow.as_ref().unwrap().play.remaining(10.5);
    assert!(remaining > 0.0);
    let pending_before = app.renderer.pending_slots();

    let hidden = viewport_raw(egui::vec2(800.0, 600.0), false);
    let _ = ctx.run_logic(&hidden, |ctx| app.logic(ctx));
    assert!(app.ui.slideshow.as_ref().unwrap().play.is_paused(), "hiding auto-pauses playback");
    app.set_simulated_clock(Some(20.0));
    let _ = ctx.run_logic(&hidden, |ctx| app.logic(ctx));
    {
        let play = &app.ui.slideshow.as_ref().unwrap().play;
        assert_eq!(play.current(), first, "a hidden slideshow does not advance");
        assert!((play.remaining(20.0) - remaining).abs() < 1e-6, "the slide keeps its remaining time while hidden");
    }
    assert_eq!(app.session.active(), selection, "a hidden slideshow does not move the selection");
    assert_eq!(app.renderer.pending_slots(), pending_before, "a hidden slideshow requests no new slide renders");

    let visible = viewport_raw(egui::vec2(800.0, 600.0), true);
    app.set_simulated_clock(Some(20.0));
    let _ = ctx.run_logic(&visible, |ctx| app.logic(ctx));
    let play = &app.ui.slideshow.as_ref().unwrap().play;
    assert!(!play.is_paused(), "showing the window resumes playback");
    assert_eq!(play.current(), first);
    assert!((play.remaining(20.0) - remaining).abs() < 1e-6, "resuming does not skip the hidden minutes");
}

#[test]
fn text_field_inherits_ime_delete_surrounding_bytes_exactly() {
    // The pinned 0.37 fork sends winit's incoming byte deletion as
    // `ImeEvent::DeleteSurroundingBytes`; the app's `TextField` wraps the built-in `TextEdit`,
    // so it must apply the range exactly (not extend it to a grapheme cluster).
    let ctx = egui::Context::default();
    let id = crate::text_field::id("ime-bytes");
    let mut text = String::from("abc");
    let pass = |events: Vec<egui::Event>, text: &mut String| {
        let raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        frame(&ctx, raw, |ui| {
            ui.memory_mut(|memory| memory.request_focus(id));
            let _ = crate::text_field::TextField::singleline("ime-bytes", text).show(ui);
        });
    };
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(3))));
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 2, after_bytes: 0 })], &mut text);
    assert_eq!(text, "a", "the reported UTF-8 range is deleted exactly");

    // The same path must not split a multi-byte codepoint when the IME reports its whole width.
    let mut text = String::from("aé🙂z");
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(3)))); // after 🙂
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::DeleteSurroundingBytes { before_bytes: 4, after_bytes: 0 })], &mut text);
    assert_eq!(text, "aéz", "the whole emoji is removed without touching the combining-free neighbours");
}

#[test]
fn text_field_ime_commit_replaces_the_selection() {
    let ctx = egui::Context::default();
    let id = crate::text_field::id("ime-commit");
    let mut text = String::from("abc");
    let pass = |events: Vec<egui::Event>, text: &mut String| {
        let raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0))), events, ..Default::default() };
        frame(&ctx, raw, |ui| {
            ui.memory_mut(|memory| memory.request_focus(id));
            let _ = crate::text_field::TextField::singleline("ime-commit", text).show(ui);
        });
    };
    pass(vec![], &mut text);
    pass(vec![], &mut text);
    let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(2))));
    state.store(&ctx, id);
    pass(vec![egui::Event::Ime(egui::ImeEvent::Commit("é".into()))], &mut text);
    assert_eq!(text, "éc", "the committed text replaces the selected range once");
}

fn shot_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-shot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp screenshot folder");
    d
}

/// Complete one admitted capture whose slot is bound to the current geometry.
fn complete_admitted(ctx: &egui::Context, app: &mut LightcraftApp, image: egui::ColorImage) {
    let generation = app.shot_document_generation();
    let issued = geometry(100.0, 100.0);
    let callback = {
        let shot = app.pending_screenshots.last_mut().expect("pending");
        issue_into_slot(ctx, shot, issued, generation)
    };
    callback.complete(Arc::new(image));
    app.collect_screenshots(ctx);
}

#[test]
fn a_new_file_at_an_absent_screenshot_path_survives() {
    let dir = shot_dir("absent-race");
    let path = dir.join("shot.png");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("the path is free");
    assert!(target.was_absent());
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    // A photo (or any other file) appears at the path while the capture is queued.
    std::fs::write(&path, b"new original").expect("created while queued");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("appeared"), "{reply}");
    assert_eq!(std::fs::read(&path).expect("the new file is still there"), b"new original");
    assert_eq!(app.shot_bytes, 0, "the reservation is released on refusal");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_absent_screenshot_path_that_becomes_a_hard_link_to_an_original_survives() {
    let dir = shot_dir("absent-original");
    let original = dir.join("IMG_1.jpg");
    let path = dir.join("shot.png");
    std::fs::write(&original, b"original").expect("original");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("the path is free");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    std::fs::hard_link(&original, &path).expect("an imported hard link appears while queued");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(std::fs::read(&original).expect("original survives"), b"original");
    assert_eq!(std::fs::read(&path).expect("the link still names the original"), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_allowed_screenshot_file_is_still_overwritten() {
    let dir = shot_dir("existing-allowed");
    let path = dir.join("shot.png");
    std::fs::write(&path, b"old screenshot").expect("old file");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("ordinary existing file");
    assert!(!target.was_absent());
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(std::fs::read(&path).expect("written"), vec![0x89, b'P', b'N', b'G']);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_screenshot_parent_retargeted_while_queued_is_not_written() {
    use std::os::unix::fs::symlink;

    let dir = shot_dir("parent-retargeted");
    let admitted = dir.join("admitted");
    let unexpected = dir.join("unexpected");
    let link = dir.join("destination");
    std::fs::create_dir(&admitted).expect("admitted destination folder");
    std::fs::create_dir(&unexpected).expect("unexpected destination folder");
    symlink(&admitted, &link).expect("destination folder link");
    let path = link.join("shot.png");
    let (mut app, seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("free path through a folder link");
    assert!(target.was_absent());
    assert!(app.session.revalidate_write_target(&target).is_ok(), "an unchanged folder link is a valid destination");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");

    // The path's spelling is unchanged, but it now resolves to another folder while the real
    // capture is pending. Re-checking only the saved resolved folder would allow this write.
    std::fs::remove_file(&link).expect("unlink only the test folder link");
    symlink(&unexpected, &link).expect("retarget within the test temp folder");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));

    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("destination folder changed"), "{reply}");
    assert!(!admitted.join("shot.png").exists(), "the refused capture writes nothing to the admitted folder");
    assert!(!unexpected.join("shot.png").exists(), "the refused capture writes nothing to the retargeted folder");
    assert!(seen.lock().unwrap().is_none(), "a changed destination is refused before encoding");
    assert!(app.pending_screenshots.is_empty());
    assert_eq!(app.shot_bytes, 0, "the refused capture releases its reservation");
    assert!(rx.try_recv().is_err(), "the refused capture has exactly one reply");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn an_existing_screenshot_target_replaced_while_queued_is_not_overwritten() {
    let dir = shot_dir("existing-replaced");
    let path = dir.join("shot.png");
    let keep = dir.join("keep.png");
    let other = dir.join("other.png");
    std::fs::write(&path, b"old screenshot").expect("old file");
    let (mut app, _seen) = app_with_screenshot_files();
    let ctx = egui::Context::default();
    app.last_shot_geometry = Some(geometry(100.0, 100.0));
    let (tx, rx) = reply_channel();
    let target = app.session.pin_write_target(&path.to_string_lossy()).expect("ordinary existing file");
    app.admit_shot(&ctx, Some(target), false, tx).expect("admitted");
    // Keep the admitted inode alive, then replace the directory entry with a different file.
    std::fs::hard_link(&path, &keep).expect("keep the admitted inode");
    std::fs::write(&other, b"new file").expect("new file");
    std::fs::rename(&other, &path).expect("replace the entry");
    complete_admitted(&ctx, &mut app, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let reply = rx.try_recv().expect("one reply");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("changed"), "{reply}");
    assert_eq!(std::fs::read(&path).expect("the replacing file survives"), b"new file");
    assert_eq!(std::fs::read(&keep).expect("the admitted file survives"), b"old screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ui_scroll_carries_validated_unit_phase_and_source() {
    let (mut app, ctx) = (app(), egui::Context::default());
    let result = crate::control::gesture_events(
        &mut app,
        &ctx,
        "ui.scroll",
        &serde_json::json!({"source": "wheel", "unit": "line", "phase": "start", "dy": 1.0}),
    )
    .expect("a wheel line step is valid");
    assert_eq!(result, serde_json::Value::Null);
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Line);
            assert_eq!(*phase, egui::TouchPhase::Start);
            assert_eq!(*source, egui::MouseWheelSource::Wheel);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    app.synthetic.clear();
    crate::control::gesture_events(
        &mut app,
        &ctx,
        "ui.scroll",
        &serde_json::json!({"source": "trackpad", "unit": "point", "phase": "end", "dy": -12.0}),
    )
    .expect("a trackpad end event is valid");
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Point);
            assert_eq!(*phase, egui::TouchPhase::End);
            assert_eq!(*source, egui::MouseWheelSource::Trackpad);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    // Omitted keys keep the pre-0.37 request meaning.
    app.synthetic.clear();
    crate::control::gesture_events(&mut app, &ctx, "ui.scroll", &serde_json::json!({"dy": 3.0})).expect("defaults are valid");
    match app.synthetic.first().expect("one wheel event") {
        egui::Event::MouseWheel { unit, phase, source, .. } => {
            assert_eq!(*unit, egui::MouseWheelUnit::Point);
            assert_eq!(*phase, egui::TouchPhase::Move);
            assert_eq!(*source, egui::MouseWheelSource::Unknown);
        }
        other => panic!("expected a wheel event, got {other:?}"),
    }

    // An invalid value is an error and queues nothing.
    app.synthetic.clear();
    let error = crate::control::gesture_events(&mut app, &ctx, "ui.scroll", &serde_json::json!({"unit": "notch"})).expect_err("unknown unit");
    assert!(error.contains("unit"), "{error}");
    assert!(app.synthetic.is_empty());
}

#[test]
fn a_gesture_never_uses_the_previous_widget_registry() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.widgets.push(("stale:test".into(), egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0))));
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.clickWidget".into(), serde_json::json!({"id": "stale:test"}), tx).expect("admitted");
    assert!(rx.try_recv().is_err(), "nothing is answered at admission");
    // A completed pass replaces the registry; the stale id is not in the fresh one.
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the resolution error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("no widget"), "{reply}");
    assert!(app.pending_gestures.is_empty());
    assert!(app.synthetic.is_empty());
}

#[test]
fn a_gesture_cancels_when_the_view_changes_before_the_fresh_layout() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 10.0, "y": 10.0}), tx).expect("admitted");
    // the app opens in Detail, so a cancellation test must move to a genuinely different view
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the cancel error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("canceled"), "{reply}");
    assert!(app.synthetic.is_empty());
}

#[test]
fn a_gesture_replies_only_after_the_completed_pass_consumed_its_event() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    app.ui.view = crate::state::ViewMode::Detail;
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 12.0, "y": 14.0}), tx).expect("admitted");
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.synthetic.len(), 1, "resolution queued the event");
    assert!(rx.try_recv().is_err(), "not answered before the UI consumed it");
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&mut next);
    assert!(next.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_))), "the event is injected next frame");
    frame(&ctx, next, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    let reply = rx.try_recv().expect("the delivery reply");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_gestures.is_empty());
}

#[test]
fn a_delivered_gesture_may_change_the_state_it_acted_on() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.key".into(), serde_json::json!({"key": "j", "cmd": true}), tx).expect("admitted");
    // The first completed pass resolves the key against the Detail layout and queues it.
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert!(!app.synthetic.is_empty(), "the key is queued");
    // The pass that consumes it opens the dialog: that is the gesture doing its job, not drift.
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&mut next);
    frame(&ctx, next, |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.ui.dialog, Some(crate::state::Dialog::ViewOptions));
    let reply = rx.try_recv().expect("the delivered gesture answers");
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(app.pending_gestures.is_empty());
}

#[test]
fn a_gesture_cancels_when_the_view_changes_before_its_batch_is_injected() {
    let ctx = egui::Context::default();
    let mut app = app();
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| app.logic(ui.ctx()));
    app.logic(&ctx);
    assert!(app.fonts_ready);
    let (tx, rx) = reply_channel();
    app.admit_gesture(&ctx, "ui.move".into(), serde_json::json!({"x": 10.0, "y": 10.0}), tx).expect("admitted");
    frame(&ctx, raw(egui::vec2(1000.0, 700.0)), |ui| {
        app.logic(ui.ctx());
        app.ui(ui);
    });
    assert_eq!(app.synthetic.len(), 1, "resolution queued the event");
    // A hidden logic tick changes what the queued pointer event would mean before a visible
    // pass consumes it: fail closed instead of injecting it into the new view.
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    let mut next = raw(egui::vec2(1000.0, 700.0));
    app.raw_input_hook(&mut next);
    assert!(next.events.is_empty(), "no stale event reaches the new view");
    assert!(app.synthetic.is_empty());
    let reply = rx.try_recv().expect("the cancel error answers once");
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().unwrap_or_default().contains("canceled"), "{reply}");
    assert!(app.pending_gestures.is_empty());
}

fn queued_shot(sent_at_ms: Option<f64>) -> PendingShot {
    let (tx, _rx) = reply_channel();
    PendingShot {
        token: 1,
        target: None,
        reply: tx,
        headless: false,
        admitted_passes: 0,
        not_before_ms: 0.0,
        force_ms: 0.0,
        deadline_ms: 10_000.0,
        sent_at_ms,
        reserved: 1,
        slot: None,
        issued: None,
        issued_library: 0,
    }
}

#[test]
fn hidden_paint_requests_only_queued_shots_not_issued_ones() {
    let mut app = app();
    app.pending_screenshots.push(queued_shot(Some(1.0)));
    assert!(!app.needs_ui_pass_while_hidden(), "an issued screenshot forces its own paint");
    app.pending_screenshots.clear();
    app.pending_screenshots.push(queued_shot(None));
    assert!(app.needs_ui_pass_while_hidden(), "a queued/settling screenshot needs a UI pass");
    app.pending_screenshots.clear();
    app.synthetic.push(egui::Event::PointerMoved(egui::Pos2::ZERO));
    assert!(app.needs_ui_pass_while_hidden(), "a gesture step needs a UI pass");
}

#[test]
fn disclosure_rows_publish_expanded_not_toggled() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut seen = Vec::new();
    frame(&ctx, raw(egui::vec2(400.0, 300.0)), |ui| {
        let (resp, _) = crate::widgets::section_header(ui, "test-section", "Light", true, None);
        seen.push(ui.ctx().accesskit_node_builder(resp.id, |node| node.is_expanded()).flatten());
    });
    frame(&ctx, raw(egui::vec2(400.0, 300.0)), |ui| {
        let (resp, _) = crate::widgets::section_header(ui, "test-section", "Light", false, None);
        seen.push(ui.ctx().accesskit_node_builder(resp.id, |node| node.is_expanded()).flatten());
    });
    assert_eq!(seen, vec![Some(true), Some(false)], "the disclosure reports Expanded, not Toggled");
}
