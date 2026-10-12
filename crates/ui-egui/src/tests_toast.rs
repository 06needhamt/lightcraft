//! The toast is drawn as widgets, not as paint. That is what makes its message selectable: a toast
//! often carries a path or a command the user wants somewhere else, and painted text cannot be
//! picked up by the pointer at all.

use std::time::Duration;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // let the first renders finish: a render still running when the test binary exits can crash in the
    // GPU driver's teardown (the preview pool doesn't join its workers)
    h.settle(T);
    h
}

/// One event per frame: [`LightcraftApp::raw_input_hook`] takes pointer events singly.
fn send(h: &mut Headless, e: egui::Event) {
    h.app.synthetic.push(e);
    h.step();
}

fn show_toast(h: &mut Headless, msg: &str) -> egui::Rect {
    h.app.ui.toast = Some((msg.to_owned(), f64::MAX, None));
    h.step();
    h.step();
    h.view.ctx.memory(|m| m.area_rect(egui::Id::new("toast"))).expect("the toast is laid out as a widget, not paint")
}

#[test]
fn the_toast_is_a_widget_the_pointer_reaches() {
    let mut h = demo();
    let msg = "Exported frame to /tmp/scene-0001.png";
    let area = show_toast(&mut h, msg);

    assert!(area.is_positive(), "the toast has a rect of its own: {area:?}");
    assert!(h.painted_text().iter().any(|t| t.contains("Exported frame")), "the message is still painted");

    // The pointer reaches the message: egui offers the text cursor over it. Painted text is never
    // hit-tested, so this is the difference that makes selection possible at all.
    send(&mut h, egui::Event::PointerMoved(area.center()));
    assert_eq!(h.last_cursor, egui::CursorIcon::Text, "the toast text is a text target");
}

#[test]
fn the_toast_still_wraps_a_long_message_inside_the_canvas() {
    let mut h = demo();
    let msg = "The SAM 3 download failed: the file is 4.1 GB and the connection dropped at 98 %, so nothing was installed and the masks are still unavailable";
    let area = show_toast(&mut h, msg);

    let canvas = h.app.canvas_rect.expect("the demo has a canvas");
    assert!(area.width() <= canvas.width() - 80.0, "the toast wraps within the canvas: {area:?}");
    assert!(h.painted_text().iter().any(|t| t.contains("SAM 3 download failed")), "the whole message is painted");
}
