//! One star rating for the whole interface: the clickable row of five stars ([`Rating`]) and the
//! painter-only helpers for stars drawn onto photos and captions ([`paint_rating`], [`paint_star`]).
//! It replaces `widgets::stars` and the private star-painting loops of the grid badge, the film
//! badges, the compare caption and the filter bar.
//!
//! ```ignore
//! if let Some(r) = Rating::new("bottom", photo.rating).size(RatingSize::Large).show(ui) { … }
//! Rating::new("row", 3).size(RatingSize::Small).read_only().show(ui);
//! ```
//!
//! Hovering a star previews the rating a click would set; clicking the current rating clears it.
//! With a star focused, ← and → lower and raise the rating (0 to 5).

use egui::{Color32, Key, Modifiers, Painter, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{announce_selected, register};

/// Number of stars in a rating.
const STARS: u8 = 5;

/// How big a rating is drawn. Each size matches one of the renderers it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingSize {
    /// Film badges: 8 px stars every 9 px.
    Small,
    /// Grid badges and compare captions: 12 px stars every 13 px.
    Medium,
    /// The bottom bar, the Info panel and the filter bar: 14 px stars in 19 px cells.
    Large,
}

impl RatingSize {
    /// Side of one painted star.
    pub fn icon(self) -> f32 {
        match self {
            Self::Small => 8.0,
            Self::Medium => 12.0,
            Self::Large => 14.0,
        }
    }

    /// Distance from one painted star to the next in [`paint_rating`].
    pub fn step(self) -> f32 {
        match self {
            Self::Small => 9.0,
            Self::Medium => 13.0,
            Self::Large => 17.0,
        }
    }

    /// Side of one clickable cell in a [`Rating`] row.
    fn cell(self) -> f32 {
        match self {
            Self::Small => 12.0,
            Self::Medium => 16.0,
            Self::Large => 19.0,
        }
    }
}

/// Paint one star (outline or filled) centred in `rect`.
pub fn paint_star(painter: &Painter, rect: Rect, filled: bool, color: Color32) {
    paint(painter, rect, if filled { Icon::StarFilled } else { Icon::Star }, color);
}

/// Paint `rating` filled stars in a row, the first one's left edge at `pos.x` and its vertical centre
/// at `pos.y`. Returns the width the row takes (0 for no stars), so the caller can lay out what follows.
pub fn paint_rating(painter: &Painter, pos: Pos2, rating: u8, size: RatingSize, color: Color32) -> f32 {
    let n = rating.min(STARS);
    let side = size.icon();
    for i in 0..n {
        let left = pos.x + f32::from(i) * size.step();
        paint_star(painter, Rect::from_min_size(pos2(left, pos.y - side / 2.0), vec2(side, side)), true, color);
    }
    f32::from(n) * size.step()
}

/// What the stars show once hovering is taken into account: the hovered star's rating previews over
/// the current one.
fn star_lit(i: u8, rating: u8, hover: Option<u8>) -> bool {
    i < hover.unwrap_or(rating)
}

/// The rating a click on star `n` (1 to 5) sets: clicking the current rating clears it.
fn clicked_rating(current: u8, n: u8) -> u8 {
    if current == n { 0 } else { n }
}

/// The rating the arrow keys move to from `current`, if any.
fn nudged_rating(current: u8, left: bool, right: bool) -> Option<u8> {
    match (left, right) {
        (true, false) if current > 0 => Some(current - 1),
        (false, true) if current < STARS => Some(current + 1),
        _ => None,
    }
}

/// A row of five stars. Build it, then [`Rating::show`].
pub struct Rating {
    id: String,
    rating: u8,
    size: RatingSize,
    read_only: bool,
    enabled: bool,
    ids_prefix: Option<String>,
}

impl Rating {
    /// A rating of `rating` (0 to 5) whose stars are reachable as `star:{id}:{n}`.
    pub fn new(id: impl Into<String>, rating: u8) -> Self {
        Self { id: id.into(), rating: rating.min(STARS), size: RatingSize::Large, read_only: false, enabled: true, ids_prefix: None }
    }

    /// The size the stars are drawn at (default [`RatingSize::Large`]).
    pub fn size(mut self, size: RatingSize) -> Self {
        self.size = size;
        self
    }

    /// Show the rating without reacting to hover or clicks.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Whether the stars react (default true). A disabled row is faded and ignores clicks.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Register the stars under `{prefix}{n}` instead of `star:{id}:{n}`, for rows that already have
    /// published automation ids (the filter bar's `filter:star3`).
    pub fn automation_ids(mut self, prefix: impl Into<String>) -> Self {
        self.ids_prefix = Some(prefix.into());
        self
    }

    fn widget_id(&self, n: u8) -> String {
        match &self.ids_prefix {
            Some(prefix) => format!("{prefix}{n}"),
            None => format!("star:{}:{n}", self.id),
        }
    }

    /// Draw the row. Returns the new rating when a click or an arrow key changed it.
    pub fn show(self, ui: &mut Ui) -> Option<u8> {
        let t = Tokens::get(ui.ctx());
        let interactive = self.enabled && !self.read_only;
        let cell = self.size.cell();
        let mut out = None;
        let mut hover_n = None;
        let mut focused = false;
        let mut cells = [Rect::NOTHING; STARS as usize];
        for (i, slot) in (0..STARS).zip(cells.iter_mut()) {
            let (r, resp) = ui.allocate_exact_size(vec2(cell, cell), if interactive { Sense::click() } else { Sense::hover() });
            announce_selected(&resp, format!("{} ★", i + 1), i < self.rating);
            register(ui.ctx(), self.widget_id(i + 1), r);
            if interactive {
                if resp.hovered() {
                    hover_n = Some(i + 1);
                }
                if resp.clicked() {
                    out = Some(clicked_rating(self.rating, i + 1));
                }
                focused |= resp.has_focus();
            }
            *slot = r;
        }
        if focused && out.is_none() {
            let (left, right) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowLeft), i.consume_key(Modifiers::NONE, Key::ArrowRight)));
            out = nudged_rating(self.rating, left, right);
        }
        // The hovered star also gets the icon-button hover square, so hovering the current rating (where
        // the preview looks the same) still shows feedback.
        let fade = if self.enabled { 1.0 } else { 0.4 };
        let side = self.size.icon();
        for (i, r) in (0..STARS).zip(cells) {
            if hover_n == Some(i + 1) {
                ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.7));
            }
            let filled = star_lit(i, self.rating, hover_n);
            let color = if filled { t.star } else { t.icon };
            paint_star(ui.painter(), Rect::from_center_size(r.center(), vec2(side, side)), filled, color.gamma_multiply(fade));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hovered_star_previews_its_rating() {
        let lit = |rating, hover| (0..STARS).filter(|&i| star_lit(i, rating, hover)).count();
        assert_eq!(lit(2, None), 2, "no hover shows the rating");
        assert_eq!(lit(2, Some(4)), 4);
        assert_eq!(lit(5, Some(1)), 1);
        assert_eq!(lit(0, Some(3)), 3);
    }

    #[test]
    fn arrows_stay_between_zero_and_five() {
        assert_eq!(nudged_rating(0, true, false), None);
        assert_eq!(nudged_rating(5, false, true), None);
        assert_eq!(nudged_rating(3, true, false), Some(2));
        assert_eq!(nudged_rating(3, false, true), Some(4));
        assert_eq!(nudged_rating(3, true, true), None);
    }
}
