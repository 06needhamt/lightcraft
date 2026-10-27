//! What a screen reader is told about the sidebar's disclosure triangles: each names the row it
//! opens or closes, so a list of them is not "Expand, Expand, Expand".
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn sidebar() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.right = RightPanel::None;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    h
}

fn button_names(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).filter_map(|n| n.accesskit_node().label()).collect()
}

#[test]
fn a_disclosure_triangle_names_its_row() {
    let h = sidebar();
    let names = button_names(&h);
    // the demo library's album folder is open, with albums inside it; By Date is closed
    assert!(names.iter().any(|n| n == "Collapse Travel 2026"), "the open album folder's triangle: {names:?}");
    assert!(names.iter().any(|n| n == "Expand 2026"), "By Date's year: {names:?}");
    assert!(!names.iter().any(|n| n == "Expand" || n == "Collapse"), "no triangle is just a verb: {names:?}");
}
