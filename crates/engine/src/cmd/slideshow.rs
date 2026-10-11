//! `slideshow.playback`: the slideshow's Playback settings (slide and fade durations, fade colour,
//! random order, repeat, preparing previews in advance). `slideshow.layout`: how a slide looks
//! (Layout, Options and Backdrop: margins, aspect preview, zoom to fill, stroke, shadow, backdrop
//! colour, colour wash, background image). Both are library preferences, saved in the library's
//! `prefs.json`; the app's slideshow (View ▸ Slideshow) plays with them. `slideshow.overlays`:
//! the identity plate, rating stars, text overlays with tokens and the watermark drawn on every
//! slide (also a library preference). `slideshow.titles`: the intro and ending screens (colour,
//! identity plate or text, scale, duration; a library preference). `slideshow.preview` composes
//! one slide, or a title screen, as a PNG.

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// Largest side `slideshow.preview` renders.
pub const PREVIEW_MAX: usize = 4096;

fn playback(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slideshow.merged(p).map_err(|e| bad("slideshow.playback", e))?;
        if next != s.slideshow {
            s.slideshow = next;
            s.save_prefs()?;
        }
    }
    Ok(s.slideshow.to_json())
}

fn layout(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_layout.merged(p).map_err(|e| bad("slideshow.layout", e))?;
        if next != s.slide_layout {
            s.slide_layout = next;
            s.save_prefs()?;
        }
    }
    Ok(s.slide_layout.to_json())
}

fn overlays(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_overlays.merged(p).map_err(|e| bad("slideshow.overlays", e))?;
        if next != s.slide_overlays {
            s.slide_overlays = next;
            s.save_prefs()?;
        }
    }
    let mut out = s.slide_overlays.to_json();
    out["warnings"] = json!(s.slide_overlays.warnings());
    Ok(out)
}

fn titles(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_titles.merged(p).map_err(|e| bad("slideshow.titles", e))?;
        if next != s.slide_titles {
            s.slide_titles = next;
            s.save_prefs()?;
        }
    }
    let mut out = s.slide_titles.to_json();
    out["warnings"] = json!(s.slide_titles.warnings(&s.slide_overlays));
    Ok(out)
}

/// A slide size from the request: both sides given, one (the other from the aspect preview), or
/// neither (1280 wide). Each side 16–[`PREVIEW_MAX`].
fn preview_size(s: &Session, p: &Value) -> Result<(usize, usize)> {
    const C: &str = "slideshow.preview";
    let side = |k: &str| -> Result<Option<usize>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_u64() {
                Some(n) if (16..=PREVIEW_MAX as u64).contains(&n) => Ok(Some(n as usize)),
                _ => Err(bad(C, format!("`{k}` must be a whole number of pixels from 16 to {PREVIEW_MAX}, not {v}"))),
            },
        }
    };
    let ratio = s.slide_layout.aspect.ratio(f64::NAN);
    let clamp = |v: f64| (v.round() as usize).clamp(16, PREVIEW_MAX);
    Ok(match (side("width")?, side("height")?) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, clamp(w as f64 / ratio)),
        (None, Some(h)) => (clamp(h as f64 * ratio), h),
        (None, None) => (1280, clamp(1280.0 / ratio)),
    })
}

/// Write `png` to `path`, or put it in the reply as base64.
fn deliver(png: &[u8], path: Option<&str>, out: &mut Value) -> Result<()> {
    match path {
        Some(path) => {
            crate::export::write_file(path, png).map_err(crate::EngineError::Other)?;
            out["path"] = json!(path);
        }
        None => out["png"] = json!(base64(png)),
    }
    Ok(())
}

fn encode_png(img: &lightcraft_raster::Rgba8) -> Result<Vec<u8>> {
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default())
        .map_err(|e| crate::EngineError::Other(format!("PNG: {e}")))
}

/// `slideshow.preview {title}`: a title screen (whether or not it is switched on).
fn preview_title(s: &mut Session, p: &Value, title: &Value) -> Result<Value> {
    const C: &str = "slideshow.preview";
    let t = title
        .as_str()
        .and_then(crate::slideshow::Title::parse)
        .ok_or_else(|| bad(C, format!("`title` must be \"intro\" or \"ending\", not {title}")))?;
    if p.get("id").is_some_and(|v| !v.is_null()) {
        return Err(bad(C, "pass `id` or `title`, not both"));
    }
    let (w, h) = preview_size(s, p)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty());
    if let Some(path) = path {
        s.check_write_target(path).map_err(|e| bad(C, e))?;
    }
    let c = s.title_compose(t, w, h);
    let img = c.compose();
    let png = encode_png(&img)?;
    let rect = |r: lightcraft_slideshow::Rect| json!([r.x0, r.y0, r.x1, r.y1]);
    let mut out = json!({
        "title": t.id(),
        "enabled": s.slide_titles.screen(t).enabled,
        "width": img.width,
        "height": img.height,
        "plate": c.plate_box().map(rect),
    });
    deliver(&png, path, &mut out)?;
    Ok(out)
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.preview";
    if let Some(t) = p.get("title").filter(|v| !v.is_null()) {
        return preview_title(s, p, t);
    }
    let id = match p.get("id") {
        None | Some(Value::Null) => s.active().ok_or_else(|| bad(C, "no photo selected (select one or pass `id`)"))?,
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(C, format!("`id` must be a photo id, not {v}")))?),
    };
    if s.catalog.photo(id).is_none() {
        return Err(bad(C, format!("no photo {}", id.0)));
    }
    let (w, h) = preview_size(s, p)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty());
    if let Some(path) = path {
        s.check_write_target(path).map_err(|e| bad(C, e))?;
    }
    let settings = s.slide_layout.clone();
    let img = s.slide_now(&settings, id, w, h).map_err(crate::EngineError::Other)?;
    let png = encode_png(&img)?;
    let aspect = s.slide_photo_aspect(id).unwrap_or(1.5);
    let l = lightcraft_slideshow::layout(&settings, aspect, img.width, img.height);
    let rect = |r: lightcraft_slideshow::Rect| json!([r.x0, r.y0, r.x1, r.y1]);
    let mut out = json!({
        "id": id.0,
        "width": img.width,
        "height": img.height,
        "cell": rect(l.cell),
        "photo": rect(l.visible),
    });
    let photo = s.catalog.photo(id).cloned();
    let o = crate::slide_overlay::SlideOverlays::resolve(&s.slide_overlays, photo.as_deref());
    out["overlays"] = o.boxes(&l).into_iter().map(|(k, r)| json!({"kind": k.id(), "box": rect(r)})).collect();
    deliver(&png, path, &mut out)?;
    Ok(out)
}

/// Standard padded base64 (for an image returned in the reply).
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let at = |shift: u32| A.get((n >> shift) as usize & 63).copied().unwrap_or(b'=') as char;
        out.push(at(18));
        out.push(at(12));
        out.push(if chunk.len() > 1 { at(6) } else { '=' });
        out.push(if chunk.len() > 2 { at(0) } else { '=' });
    }
    out
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "slideshow.playback",
            "Slideshow Playback Settings",
            [],
            None,
            "{slideDuration?: seconds (0.5–120), fadeDuration?: seconds (0 = cut; at most 10 and the slide duration), fadeColor?: \"#rrggbb\"|[r,g,b]|null (fade through a colour instead of crossfading), randomOrder?: bool, repeat?: bool, preparePreviews?: bool (render the next slides before they are due)} — omitted keys keep their value, {} reads them; saved with the library → {slideDuration, fadeDuration, fadeColor, randomOrder, repeat, preparePreviews}",
            always,
            playback
        ),
        cmd!(
            "slideshow.layout",
            "Slideshow Layout Settings",
            [],
            None,
            "{margins?: all four, marginLeft?, marginRight?, marginTop?, marginBottom?: fractions of the slide's shorter side (0–0.45; with linkMargins one sets all four), linkMargins?: bool, showGuides?: bool (margin guides in the slide editor), aspect?: \"screen\"|\"16:9\"|\"4:3\" (aspect preview), zoomToFill?: bool (crop the photo to fill its cell), strokeBorder?: bool, strokeColor?: \"#rrggbb\"|[r,g,b], strokeWidth?: fraction of the shorter side (0–0.05), castShadow?: bool, shadowOpacity?: 0–1, shadowOffset?: 0–0.1, shadowRadius?: 0–0.1 (fractions of the shorter side), shadowAngle?: degrees counter-clockwise from the right (−45 = down right), backdropColor?: colour, colorWash?: bool, washColor?: colour, washOpacity?: 0–1, washAngle?: degrees (the side the wash colour is full at), backgroundImage?: photo id|null, backgroundOpacity?: 0–1} — omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library → the settings",
            always,
            layout
        ),
        cmd!(
            "slideshow.overlays",
            "Slideshow Overlay Settings",
            [],
            None,
            "{identityPlate?: {enabled?, text?, image?: PNG/JPEG file (drawn instead of the text; \"\" = the text), size?: height as a fraction of the slide's shorter side (0.005–0.5), color?: \"#rrggbb\"|[r,g,b], opacity?: 0–1, …placement, behind?: bool (render behind the photo)}, ratingStars?: {enabled?, color?, size?: a star's height (0.005–0.15), …placement}, texts?: [text overlay] (replaces the list, at most 16; each entry merged onto the one at its index), addText?: text overlay ({} = the default \"{file}\"), removeText?: index, showWatermark?: bool, watermark?: text | the export watermark object {text, vertical, size, opacity, anchor, inset, color, shadow, image, imageWidth} (merged; sizes relative to the photo)} where a text overlay is {text?: template with tokens ({file}, {name}, {title}, {caption}, {date} (YYYY-MM-DD) / {date:%d.%m.%Y}, {exposure}, {shutter}, {aperture}, {focal}, {iso}, {camera}, {lens}, {rating}, {creator}, {copyright}, {folder}, {ext}; unknown tags show as typed), size?: line height (0.005–0.3), color?, opacity?, …placement, shadow?: bool, shadowOpacity?: 0–1, shadowOffset?, shadowRadius?: 0–0.05 (fractions of the shorter side), shadowAngle?: degrees counter-clockwise from the right} and the placement keys are anchor?: topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight, offsetX?, offsetY?: fractions of the slide's shorter side (x right, y down; −1–1), relativeTo?: \"slide\"|\"photo\" (the overlay is kept on the slide) — omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library; slideshow playback and slideshow.preview draw them → the settings and warnings (an unreadable identity plate image, unknown tokens)",
            always,
            overlays
        ),
        cmd!(
            "slideshow.titles",
            "Slideshow Title Settings",
            [],
            None,
            "{intro?: screen, ending?: screen} where a screen is {enabled?: bool, color?: background \"#rrggbb\"|[r,g,b], content?: \"none\"|\"identityPlate\" (the Overlays identity plate, its text or image)|\"text\", text?: the text shown with content \"text\", overrideColor?: bool (draw the plate or text in textColor instead of the identity plate's colour; an image plate keeps its colours), textColor?: colour, scale?: height of the plate or text line as a fraction of the slide's shorter side (0.005–0.5; text wider than the slide shrinks to fit), duration?: seconds (0.5–120) | null (the slide duration)} — the intro plays once before the first slide, the ending after the last one when repeat is off; both fade with the playback fade; random order never moves them. Omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library → the settings and warnings (an unreadable identity plate image, a screen with nothing to show)",
            always,
            titles
        ),
        cmd!(
            query "slideshow.preview",
            "Slideshow Slide Preview",
            [],
            None,
            "{id?: photo (default: the active one) | title?: \"intro\"|\"ending\", width?, height?: pixels (16–4096; one given: the other from the aspect preview; neither: 1280 wide), path?: write the PNG here} — the photo's slide composed with the slideshow layout and overlays → {id, width, height, cell: [x0,y0,x1,y1], photo: [x0,y0,x1,y1] (the visible photo, slide pixels), overlays: [{kind: identityPlate|ratingStars|textN|watermark, box: [x0,y0,x1,y1]}], path | png (base64 PNG)}; with `title`, that title screen (`slideshow.titles`; drawn even while it is off) → {title, enabled, width, height, plate: [x0,y0,x1,y1] | null (the plate or text), path | png}",
            always,
            preview
        ),
    ]
}
