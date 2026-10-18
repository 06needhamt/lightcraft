//! Render a screenshot of the masking view with the all-masks overlay on: several visible masks,
//! each outlined, only the selected one draggable. Used to attach a visual to PR #2210.
//!
//! Run: `cargo run -p lightcraft-ui-egui --example mask_overlay_shot -- <out.png>`

use std::time::Duration;

use lightcraft_raster::Rgba8;
use serde_json::json;

use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::{LightcraftApp, Services};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "mask-overlay.png".into());
    let t = Duration::from_secs(20);

    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let _ = h.request("ui.set", json!({"view": "detail"}), t);
    let _ = h.request("engine.execute", json!({"command": "panel.masking"}), t);

    // A few visible masks; mask.add selects the newest, so the last one is selected.
    for params in [
        json!({"kind": "radial", "center": [0.3, 0.4], "rx": 0.12, "ry": 0.12}),
        json!({"kind": "linear", "start": [0.65, 0.25], "end": [0.65, 0.6]}),
        json!({"kind": "radial", "center": [0.7, 0.65], "rx": 0.1, "ry": 0.14, "angle": 30}),
    ] {
        let _ = h.request("engine.execute", json!({"command": "mask.add", "params": params}), t);
    }
    let _ = h.request("engine.execute", json!({"command": "view.maskOverlayAll"}), t);
    let _ = h.request("engine.execute", json!({"command": "view.maskPins", "params": {"show": false}}), t);
    h.settle(Duration::from_secs(120));

    let px = h.view.paint(&Default::default());
    let rgba = Rgba8::from_fn(px.width(), px.height(), |x, y| {
        let p = px.pixels[y * px.width() + x];
        [p.r(), p.g(), p.b(), p.a()]
    });

    let file = std::fs::File::create(&out).expect("create png");
    let mut enc = png::Encoder::new(file, rgba.width as u32, rgba.height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(&rgba.as_bytes()).expect("encode png");
    println!("wrote {out} ({}, {})", rgba.width, rgba.height);
}
