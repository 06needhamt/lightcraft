//! The star rating: clicking sets the rating, clicking the current one clears it, a read-only or
//! disabled row ignores clicks, the arrow keys step it, and screen readers hear every star.

use egui::{Pos2, Rect, vec2};

use crate::rating::{Rating, RatingSize};

/// Draw `build` for a few frames. After the first, click the centre of star `click` (1 to 5) if given,
/// then press `keys`. Returns every value `build` reported and the AccessKit
/// (role, name) pairs.
fn drive(click: Option<u8>, keys: &[egui::Key], build: impl Fn(&mut egui::Ui) -> Option<u8>) -> (Vec<u8>, Vec<(String, String)>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut reported = Vec::new();
    let mut nodes = Vec::new();
    let mut rects: Vec<(String, Rect)> = Vec::new();
    for frame in 0..6 {
        let mut events = Vec::new();
        if frame == 1
            && let Some(n) = click
        {
            let want = format!("star:t:{n}");
            let at = rects.iter().find(|(id, _)| *id == want).map(|(_, r)| r.center()).unwrap_or(Pos2::ZERO);
            events.push(egui::Event::PointerMoved(at));
            events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
            events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        }
        // one key per frame from the third on, so Tab has moved the focus before an arrow arrives
        if frame >= 2
            && let Some(k) = keys.get(frame - 2)
        {
            events.push(egui::Event::Key { key: *k, physical_key: Some(*k), pressed: true, repeat: false, modifiers: Default::default() });
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            if let Some(v) = build(ui) {
                reported.push(v);
            }
        });
        out.textures_delta.clear();
        let seen = crate::widgets::take_registry(&ctx);
        if rects.is_empty() {
            rects = seen;
        }
        if let Some(update) = out.platform_output.accesskit_update.take() {
            nodes = update.nodes.iter().map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string())).collect();
        }
    }
    (reported, nodes)
}

fn row(rating: u8) -> Rating {
    Rating::new("t", rating).size(RatingSize::Large)
}

#[test]
fn clicking_a_star_sets_that_rating() {
    let (got, _) = drive(Some(4), &[], |ui| row(2).show(ui));
    assert_eq!(got, vec![4]);
}

#[test]
fn clicking_the_current_rating_clears_it() {
    let (got, _) = drive(Some(3), &[], |ui| row(3).show(ui));
    assert_eq!(got, vec![0]);
}

#[test]
fn a_read_only_row_ignores_clicks() {
    let (got, _) = drive(Some(4), &[], |ui| row(2).read_only().show(ui));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn a_disabled_row_ignores_clicks() {
    let (got, _) = drive(Some(4), &[], |ui| row(2).enabled(false).show(ui));
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn hovering_alone_changes_nothing() {
    // the preview is only painted; the rating is reported on click
    let (got, _) = drive(None, &[], |ui| row(2).show(ui));
    assert!(got.is_empty());
}

#[test]
fn a_screen_reader_hears_every_star_with_its_state() {
    let (_, nodes) = drive(None, &[], |ui| row(2).show(ui));
    for n in 1..=5 {
        let name = format!("{n} ★");
        assert!(nodes.iter().any(|(role, label)| role == "Button" && *label == name), "{name}: {nodes:?}");
    }
}

#[test]
fn the_arrow_keys_step_the_rating_of_a_focused_star() {
    // Tab focuses the first star; the row shows 3, so → asks for 4 and ← for 2
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::ArrowRight], |ui| row(3).show(ui));
    assert_eq!(got, vec![4], "{got:?}");
    let (got, _) = drive(None, &[egui::Key::Tab, egui::Key::ArrowLeft], |ui| row(3).show(ui));
    assert_eq!(got, vec![2], "{got:?}");
}

#[test]
fn arrow_keys_do_nothing_when_no_star_is_focused() {
    let (got, _) = drive(None, &[egui::Key::ArrowRight], |ui| row(3).show(ui));
    assert!(got.is_empty(), "{got:?}");
}
