//! Headless tests of Guided Upright guide selection and Delete (issue: remove one guide): a click
//! near a guide selects it, Delete removes the selected guide alone, a click away clears the
//! selection, and with no selection Delete removes nothing (never the photo).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Crop with Guided Upright on and two horizontal guides drawn (in lens-corrected coordinates).
fn guided() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "tool.guidedUpright"})),
        ("engine.execute", json!({"command": "geometry.guides", "params": {"guides": [[0.2, 0.3, 0.8, 0.3], [0.2, 0.7, 0.8, 0.7]]}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

fn guides_len(h: &Headless) -> usize {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.geometry.guides.len()
}

fn guide_at(h: &Headless, i: usize) -> egui::Pos2 {
    h.app.widgets.iter().rev().find(|(w, _)| *w == format!("uprightGuide:{i}")).map(|(_, r)| r.center()).expect("the guide is on screen")
}

/// A click near a guide selects it (Delete has a visible target); the guides stay put.
#[test]
fn a_click_near_a_guide_selects_it() {
    let mut h = guided();
    assert_eq!(guides_len(&h), 2);
    let id = h.app.session.active().expect("a photo");
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, Some((id.0, 0)), "the clicked guide is selected");
    assert_eq!(guides_len(&h), 2, "selecting removes nothing");
}

/// Delete removes the selected guide alone; the other one stays, and nothing stays selected.
#[test]
fn delete_removes_the_selected_guide() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 1).x, "y": guide_at(&h, 1).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide.map(|(_, i)| i), Some(1));
    let r = h.request("ui.key", json!({"key": "Delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 1, "one guide is gone");
    assert_eq!(h.app.ui.selected_guide, None, "and nothing is selected afterwards");
    // the survivor is the first guide: the second one's endpoints are gone
    let id = h.app.session.active().expect("a photo");
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[0];
    assert!(
        (a.x - 0.2).abs() < 1e-6 && (a.y - 0.3).abs() < 1e-6 && (b.x - 0.8).abs() < 1e-6 && (b.y - 0.3).abs() < 1e-6,
        "the first guide survived: {a:?}–{b:?}"
    );
}

/// A click away from every guide clears the selection (and removes nothing).
#[test]
fn a_click_away_clears_the_selection() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.selected_guide.is_some());
    let img = h.app.image_rect.expect("the photo on screen");
    // below both guides, far from them
    let r = h.request("ui.click", json!({"x": img.center().x, "y": img.top() + 0.95 * img.height()}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, None);
    assert_eq!(guides_len(&h), 2, "and removes nothing");
}

/// With nothing selected Delete removes no guide and never deletes the photo: the tool is open.
#[test]
fn delete_with_no_selection_removes_nothing() {
    let mut h = guided();
    let undo = h.app.session.undo.len();
    let r = h.request("ui.key", json!({"key": "Delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 2, "no guide is removed");
    assert_eq!(h.app.session.undo.len(), undo, "no edit either");
    assert!(h.app.session.active().is_some(), "the photo is still active");
}

/// Drawing a new guide clears the selection first, so Delete after a draw removes the new guide.
#[test]
fn drawing_a_new_guide_clears_the_selection() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.selected_guide.is_some());
    let img = h.app.image_rect.expect("the photo on screen");
    // a drag across the middle of the photo (clear of both guides) draws a third guide
    let r = h.request(
        "ui.drag",
        json!({"x": img.left() + 0.3 * img.width(), "y": img.top() + 0.5 * img.height(), "toX": img.left() + 0.7 * img.width(), "toY": img.top() + 0.5 * img.height()}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 3, "the drawn guide is added");
    assert_eq!(h.app.ui.selected_guide, None, "and the click's selection is gone");
}
