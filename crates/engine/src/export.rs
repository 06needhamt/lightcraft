//! Export: encode a rendered image to JPEG / PNG / TIFF / WebP / AVIF / JPEG XL (or copy the original / write a
//! DNG, each with the edits in XMP) in sRGB, Display P3, Adobe RGB
//! (1998) compatible, ProPhoto RGB or Rec. 2020 with an embedded ICC profile we generate from the
//! published primaries and curves, optional output sharpening and a JPEG file-size limit. Pure (bytes in, bytes out) so the desktop
//! app, CLI, MCP and the web build share it; writing the file is the caller's job.

use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, JxlColour, JxlOptions, NamedSpace, Samples, encode, icc};
pub use lightcraft_codecs::{JxlEffort, TiffCompression};
use lightcraft_meta::{DateTime, Gps, Metadata};
pub use lightcraft_pipeline::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Jpeg,
    Png,
    Tiff,
    /// A flat PSD (8/16-bit, no layers): what an image editor such as PhotoCraft saves back to.
    Psd,
    Webp,
    Avif,
    /// JPEG XL: lossy (VarDCT) or lossless, 8/16-bit, HDR as Rec. 2020 PQ.
    Jxl,
    /// The original file, unchanged, with an XMP sidecar holding the edits.
    Original,
    /// A DNG of the raw data (raw photos only), the edits embedded as XMP.
    Dng,
    /// Render to DNG: a linear DNG (16-bit float) of the rendered photo, the edit baked in.
    RenderedDng,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Self::Jpeg,
            "png" => Self::Png,
            "tiff" | "tif" => Self::Tiff,
            "psd" => Self::Psd,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "jxl" | "jpegxl" | "jpeg-xl" | "jpeg_xl" => Self::Jxl,
            "original" => Self::Original,
            "dng" => Self::Dng,
            "rendereddng" | "renderdng" | "render-dng" | "lineardng" => Self::RenderedDng,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Tiff => "tif",
            Self::Psd => "psd",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            // the original keeps its own extension (see `ExportOptions::file_name_for`)
            Self::Original => "",
            Self::Dng | Self::RenderedDng => "dng",
        }
    }

    /// Formats that render pixels (everything but `Original` and `Dng`).
    pub fn is_rendered(self) -> bool {
        !matches!(self, Self::Original | Self::Dng)
    }
}

/// How the output size is chosen (see [`Resize`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResizeMode {
    /// `value` = long edge in pixels.
    #[default]
    LongEdge,
    /// `value` = short edge in pixels.
    ShortEdge,
    /// `value` = width in pixels.
    Width,
    /// `value` = height in pixels.
    Height,
    /// Fit inside `value` wide × `height` tall as the photo stands: width ≤ `value`, height ≤
    /// `height` (Lightroom's "Width & Height").
    WidthHeight,
    /// Fit inside `value × height` pixels, whichever way round the photo is (long edge ≤ the
    /// larger; Lightroom's "Dimensions").
    Dimensions,
    /// `value` = megapixels.
    Megapixels,
    /// `value` = percent of the full size.
    Percent,
}

/// Output size: a mode, its value(s) and whether smaller photos are left at their own size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Resize {
    pub mode: ResizeMode,
    pub value: f32,
    /// Second dimension for [`ResizeMode::Dimensions`] and [`ResizeMode::WidthHeight`].
    pub height: u32,
    /// Never upscale past the photo's own (cropped) size.
    pub dont_enlarge: bool,
}

impl Default for Resize {
    fn default() -> Self {
        Self { mode: ResizeMode::LongEdge, value: 2048.0, height: 2048, dont_enlarge: true }
    }
}

/// The most pixels an export enlarged past its photo's own size can have: 512 megapixels, also
/// Lightroom's ceiling. A full-size or smaller export is never held to it.
pub const MAX_ENLARGED_PIXELS: f64 = 512e6;

impl Resize {
    pub fn long_edge(px: u32) -> Self {
        Self { value: px as f32, ..Default::default() }
    }

    /// Scale factor from a `w × h` (cropped, full-resolution) photo to the output.
    pub fn scale(&self, w: f64, h: f64) -> f64 {
        let (w, h) = (w.max(1.0), h.max(1.0));
        let (long, short) = (w.max(h), w.min(h));
        let v = (self.value as f64).max(0.0);
        let k = match self.mode {
            ResizeMode::LongEdge => v / long,
            ResizeMode::ShortEdge => v / short,
            ResizeMode::Width => v / w,
            ResizeMode::Height => v / h,
            ResizeMode::WidthHeight => (v / w).min(self.height as f64 / h),
            ResizeMode::Dimensions => {
                let (a, b) = (v, self.height as f64);
                (a.max(b) / long).min(a.min(b) / short)
            }
            ResizeMode::Megapixels => (v * 1e6 / (w * h)).sqrt(),
            ResizeMode::Percent => v / 100.0,
        };
        let k = if self.dont_enlarge { k.min(1.0) } else { k };
        // an enlargement stops at 512 megapixels (Lightroom's ceiling too): 20000 typed for 2000
        // with Don't Enlarge off asked for gigapixels, more memory than a computer has
        let k = k.min((MAX_ENLARGED_PIXELS / (w * h)).sqrt().max(1.0));
        // at least one pixel, at most the encoders' 65535 limit; the limit wins for a photo more
        // than 65535 times as long as it is short (where `clamp` would panic: min above max)
        k.max(1.0 / short).min(65_535.0 / long)
    }

    /// Output size for a `w × h` (cropped, full-resolution) photo.
    pub fn apply(&self, w: f64, h: f64) -> (usize, usize) {
        let k = self.scale(w, h);
        (((w * k).round() as usize).max(1), ((h * k).round() as usize).max(1))
    }
}

/// What happens when an output file already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Conflict {
    /// Add `-2`, `-3`, … to the name.
    #[default]
    Unique,
    Overwrite,
    /// Leave the existing file and skip the photo.
    Skip,
}

/// Where a batch's files go (Lightroom Classic's Export To).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DestinationMode {
    /// One folder for the whole batch ([`Destination::dir`]).
    #[default]
    SpecificFolder,
    /// Each photo's own folder: the folder its original is in. A photo without one on disk (a
    /// demo photo, a photo in the browser's storage) or whose folder is gone gets an error.
    SameAsOriginal,
}

impl DestinationMode {
    /// `specificFolder` / `sameAsOriginal` (any case).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "specificfolder" => Some(Self::SpecificFolder),
            "sameasoriginal" => Some(Self::SameAsOriginal),
            _ => None,
        }
    }
}

impl Conflict {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "unique" => Some(Self::Unique),
            "overwrite" => Some(Self::Overwrite),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// A named set of `app.export` params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub params: serde_json::Value,
}

/// The built-in export presets (File → Export with Preset).
pub fn builtin_presets() -> Vec<ExportPreset> {
    let p = |name: &str, params: serde_json::Value| ExportPreset { name: name.into(), params };
    vec![
        p("JPEG (Small)", serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 2048, "colorSpace": "srgb"})),
        p("JPEG (Large)", serde_json::json!({"format": "jpeg", "quality": 92, "longEdge": 0, "colorSpace": "srgb"})),
        p("Original + Settings", serde_json::json!({"format": "original"})),
        p("DNG", serde_json::json!({"format": "dng"})),
    ]
}

/// Output sharpening target (applied after resizing, in display-encoded values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharpenFor {
    #[default]
    None,
    Screen,
    Matte,
    Glossy,
}

impl SharpenFor {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "screen" => Some(Self::Screen),
            "matte" => Some(Self::Matte),
            "glossy" => Some(Self::Glossy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharpenAmount {
    Low,
    #[default]
    Standard,
    High,
}

impl SharpenAmount {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "standard" => Some(Self::Standard),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// Where a watermark sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

/// A text watermark, sized relative to the image so every export size looks the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Watermark {
    pub text: String,
    /// Basic Japanese vertical lettering: upright glyphs in columns from right to left.
    pub vertical: bool,
    /// Text height as a fraction of the image's short edge.
    pub size: f32,
    /// 0..1.
    pub opacity: f32,
    pub anchor: Anchor,
    /// Margin from the left or right edge, as a fraction of the short edge (unused when the anchor
    /// is centred across: top, center, bottom).
    pub inset_x: f32,
    /// Margin from the top or bottom edge, as a fraction of the short edge (unused when the anchor
    /// is centred down: left, center, right).
    pub inset_y: f32,
    /// Degrees clockwise: 0, 90, 180 or 270 (anything else draws unrotated). The watermark is
    /// sized before it is turned, and the insets are measured to the turned watermark.
    pub rotation: u16,
    /// sRGB colour.
    pub color: [u8; 3],
    /// Soft dark drop shadow for legibility on bright areas.
    pub shadow: bool,
    /// A graphic (PNG with transparency, JPEG, …) drawn instead of the text (empty = text).
    pub image: String,
    /// The graphic's width as a fraction of the photo's width.
    pub image_width: f32,
    /// The colour space the photo is in (set by the encoder; the graphic is converted into it).
    #[serde(skip)]
    pub target: Option<OutputSpace>,
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            text: String::new(),
            vertical: false,
            size: 0.035,
            opacity: 0.7,
            anchor: Anchor::BottomRight,
            inset_x: 0.025,
            inset_y: 0.025,
            rotation: 0,
            color: [255; 3],
            shadow: true,
            image: String::new(),
            image_width: 0.2,
            target: None,
        }
    }
}

impl Watermark {
    /// `rotation` in clockwise quarter turns.
    fn quarter_turns(&self) -> u8 {
        match self.rotation {
            90 => 1,
            180 => 2,
            270 => 3,
            _ => 0,
        }
    }
}

/// Where the point `(x, y)` of a `w × h` box lands when the box is turned `turns` quarter turns
/// clockwise (for pixel coordinates: the turned box is `h × w` for odd turns).
fn turn(turns: u8, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    match turns {
        1 => (h - 1.0 - y, x),
        2 => (w - 1.0 - x, h - 1.0 - y),
        3 => (y, w - 1.0 - x),
        _ => (x, y),
    }
}

#[cfg(test)]
use crate::text::{TEXT_FONT as WATERMARK_FONT, cell_glyphs as watermark_cell_glyphs, faces as watermark_fonts};

/// Draw `wm` onto `img` (straight alpha blending of the encoded values).
pub fn draw_watermark(img: &mut Rgba8, wm: &Watermark) {
    blend_watermark(img, wm, 1.0);
}

/// Draw `wm` onto `img`, a preview of an export whose short edge is `export_short_edge` pixels.
/// The smallest text size and shadow offset scale with the preview, so it looks like the file.
pub fn draw_watermark_preview(img: &mut Rgba8, wm: &Watermark, export_short_edge: usize) {
    let short = img.width.min(img.height);
    let scale = if export_short_edge == 0 { 1.0 } else { short as f32 / export_short_edge as f32 };
    blend_watermark(img, wm, scale);
}

fn blend_watermark(img: &mut Rgba8, wm: &Watermark, floor_scale: f32) {
    let width = img.width;
    watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, floor_scale, |x, y, k, col| {
        let p = &mut img.data[y * width + x];
        for c in 0..3 {
            p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
        }
    });
}

/// Draw `wm` into the `w × h` area of `img` whose top-left corner is (`x0`, `y0`), sized and
/// anchored as on an image of that size (a slideshow slide's photo). The area is clipped to `img`.
pub fn draw_watermark_in(img: &mut Rgba8, wm: &Watermark, x0: usize, y0: usize, w: usize, h: usize) {
    let (width, height) = (img.width, img.height);
    watermark_coverage(w, h, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
        let (px, py) = (x0.saturating_add(x), y0.saturating_add(y));
        if px >= width || py >= height {
            return;
        }
        if let Some(p) = img.data.get_mut(py * width + px) {
            for c in 0..3 {
                p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
            }
        }
    });
}

/// Draw `wm` onto a high-bit-depth image (`wm.color` is in the image's space, 8-bit encoded; float
/// images are linear and blend in linear light).
pub fn draw_watermark_deep(img: &mut DeepImage, wm: &Watermark) {
    let (width, trc) = (img.width, img.space.trc());
    match &mut img.samples {
        DeepSamples::U16(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let p = v[i + c] as f32;
                v[i + c] = (p + (col[c] as f32 * 257.0 - p) * k).round().clamp(0.0, 65535.0) as u16;
            }
        }),
        DeepSamples::F32(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let target = trc.decode(col[c] as f32 / 255.0);
                v[i + c] += (target - v[i + c]) * k;
            }
        }),
    }
}

/// Lay out `wm` on a `width × height` image and call `blend(x, y, coverage × opacity, colour)` for
/// every covered pixel (shadow pass first). `craft` is [`crate::fonts::CRAFT_FONTS`] (a parameter
/// so tests can render without it). `floor_scale` is this image's size over the exported file's
/// (1 when drawing the file itself): the 6 px minimum text size is a minimum in the file.
fn watermark_coverage(
    width: usize,
    height: usize,
    wm: &Watermark,
    craft: &'static [crate::fonts::CraftFont],
    floor_scale: f32,
    mut blend_px: impl FnMut(usize, usize, f32, [u8; 3]),
) {
    if !wm.image.trim().is_empty() {
        logo_coverage(width, height, wm, blend_px);
        return;
    }
    let text = wm.text.trim();
    if text.is_empty() || width == 0 || height == 0 {
        return;
    }
    let short = width.min(height) as f32;
    let px = (wm.size.clamp(0.005, 0.5) * short).max(6.0 * floor_scale);
    let Some(layout) = crate::text::TextLayout::new(text, px, wm.vertical, craft) else { return };
    let (tw, th) = (layout.width, layout.height);
    // laid out unturned in a tw × th box, placed by the box it becomes
    let turns = wm.quarter_turns();
    let (bw, bh) = if turns % 2 == 1 { (th, tw) } else { (tw, th) };
    let (ix, iy) = (wm.inset_x.clamp(0.0, 0.4) * short, wm.inset_y.clamp(0.0, 0.4) * short);
    let (w, h) = (width as f32, height as f32);
    use Anchor::*;
    let ox = match wm.anchor {
        TopLeft | Left | BottomLeft => ix,
        Top | Center | Bottom => (w - bw) / 2.0,
        TopRight | Right | BottomRight => w - ix - bw,
    };
    let oy = match wm.anchor {
        TopLeft | Top | TopRight => iy,
        Left | Center | Right => (h - bh) / 2.0,
        BottomLeft | Bottom | BottomRight => h - iy - bh,
    };
    let alpha = wm.opacity.clamp(0.0, 1.0);
    let passes: &[(f32, [u8; 3], f32)] =
        if wm.shadow { &[((px * 0.05).max(floor_scale), [0, 0, 0], 0.45), (0.0, wm.color, 1.0)] } else { &[(0.0, wm.color, 1.0)] };
    for &(off, col, a) in passes {
        let mut put = |gx: i32, gy: i32, cov: f32| {
            if gx < 0 || gy < 0 || gx >= width as i32 || gy >= height as i32 {
                return;
            }
            blend_px(gx as usize, gy as usize, (cov * a * alpha).clamp(0.0, 1.0), col);
        };
        if turns == 0 {
            layout.draw_in(ox + off, oy + off, w, h, &mut put);
        } else {
            // turned with the box; the shadow still falls down and to the right
            layout.draw_points(|x, y, cov| {
                let (x, y) = turn(turns, x, y, tw, th);
                put((ox + off + x) as i32, (oy + off + y) as i32, cov);
            });
        }
    }
}

/// A watermark graphic, decoded once per file (and again when the file changes).
pub(crate) fn watermark_logo(path: &str) -> Option<std::sync::Arc<Rgba8>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static CACHE: Mutex<Option<HashMap<String, (std::time::SystemTime, Arc<Rgba8>)>>> = Mutex::new(None);
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = c.get_or_insert_with(HashMap::new);
    if let Some((m, img)) = map.get(path)
        && *m == modified
    {
        return Some(img.clone());
    }
    let img = Arc::new(read_graphic(path).ok()?);
    map.insert(path.to_string(), (modified, img.clone()));
    Some(img)
}

/// A graphic file (a PNG with transparency, typically) as 8-bit sRGB with its alpha: the
/// watermark logo, the loupe's layout image. Applies EXIF orientation before either consumer sizes it.
/// The error says why it couldn't be read or decoded.
pub fn read_graphic(path: &str) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let decoded = lightcraft_codecs::decode(&bytes, Default::default()).map_err(|e| format!("can't decode {path}: {e}"))?;
    Ok(decoded.to_srgb8().into_oriented(lightcraft_geom::Orientation::from_exif(decoded.orientation)))
}

/// The graphic watermark: scaled (premultiplied, so transparent edges stay clean) to
/// `image_width` of the photo, placed like the text, blended with its alpha × opacity.
fn logo_coverage(width: usize, height: usize, wm: &Watermark, mut blend_px: impl FnMut(usize, usize, f32, [u8; 3])) {
    let Some(logo) = watermark_logo(wm.image.trim()) else {
        log::warn!("watermark: can't read {}", wm.image);
        return;
    };
    if width == 0 || height == 0 || logo.width == 0 || logo.height == 0 {
        return;
    }
    let turns = wm.quarter_turns();
    // Reserve the inset on anchored axes before fitting the turned box. Centered axes
    // ignore insets, just as placement below does. One scale preserves the graphic's shape.
    let short = width.min(height) as f32;
    let (ix, iy) = (wm.inset_x.clamp(0.0, 0.4) * short, wm.inset_y.clamp(0.0, 0.4) * short);
    use Anchor::*;
    let available_w = width as f64 - if matches!(wm.anchor, Top | Center | Bottom) { 0.0 } else { ix as f64 };
    let available_h = height as f64 - if matches!(wm.anchor, Left | Center | Right) { 0.0 } else { iy as f64 };
    let (max_w, max_h) = if turns % 2 == 1 { (available_h, available_w) } else { (available_w, available_h) };
    let scale =
        (wm.image_width.clamp(0.01, 1.0) as f64 * width as f64 / logo.width as f64).min(max_w / logo.width as f64).min(max_h / logo.height as f64);
    let lw = (logo.width as f64 * scale).round().max(1.0) as usize;
    let lh = (logo.height as f64 * scale).round().max(1.0) as usize;
    let pre: lightcraft_raster::Image<[f32; 4]> = lightcraft_raster::Image::from_fn(logo.width, logo.height, |x, y| {
        let p = logo.get(x, y);
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    });
    let scaled = lightcraft_raster::resample::resize(&pre, lw, lh, lightcraft_raster::resample::Filter::Mitchell);
    let (bw, bh) = if turns % 2 == 1 { (lh, lw) } else { (lw, lh) };
    let (w, h, lwf, lhf) = (width as f32, height as f32, bw as f32, bh as f32);
    let ox = match wm.anchor {
        TopLeft | Left | BottomLeft => ix,
        Top | Center | Bottom => (w - lwf) / 2.0,
        TopRight | Right | BottomRight => w - ix - lwf,
    }
    .max(0.0) as usize;
    let oy = match wm.anchor {
        TopLeft | Top | TopRight => iy,
        Left | Center | Right => (h - lhf) / 2.0,
        BottomLeft | Bottom | BottomRight => h - iy - lhf,
    }
    .max(0.0) as usize;
    let space = wm.target.unwrap_or(OutputSpace::Srgb);
    let op = wm.opacity.clamp(0.0, 1.0);
    for y in 0..lh {
        for x in 0..lw {
            let [r, g, b, a] = scaled.get(x, y);
            let a = a.clamp(0.0, 1.0);
            let (tx, ty) = turn(turns, x as f32, y as f32, lw as f32, lh as f32);
            let (px, py) = (ox + tx as usize, oy + ty as usize);
            if a <= 1e-4 || px >= width || py >= height {
                continue;
            }
            let c = [r, g, b].map(|v| (v / a).round().clamp(0.0, 255.0) as u8);
            blend_px(px, py, a * op, srgb8_in(space, c));
        }
    }
}

/// Which metadata is embedded in exported files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataPolicy {
    /// Everything we know: camera, capture settings, lens, location, title, caption, keywords, copyright.
    #[default]
    All,
    /// Everything except camera/lens make, model and capture settings.
    AllExceptCamera,
    /// Copyright and creator only.
    Copyright,
    /// Nothing (the sRGB profile is still embedded).
    None,
}

impl MetadataPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "allexceptcamera" => Some(Self::AllExceptCamera),
            "copyright" => Some(Self::Copyright),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Transfer curve of an HDR AVIF export (`hdrTransfer`): PQ (absolute, SDR white at 203 cd/m²) or
/// HLG (relative to the display; BT.2100 with SDR white at signal 0.75).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HdrCurve {
    #[default]
    Pq,
    Hlg,
}

impl HdrCurve {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pq" => Some(Self::Pq),
            "hlg" => Some(Self::Hlg),
            _ => None,
        }
    }
    fn codec(self) -> lightcraft_codecs::HdrTransfer {
        match self {
            Self::Pq => lightcraft_codecs::HdrTransfer::Pq,
            Self::Hlg => lightcraft_codecs::HdrTransfer::Hlg,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// 1–100 (JPEG, AVIF, lossy JPEG XL).
    pub quality: u8,
    /// Output size (`None` = full size).
    pub resize: Option<Resize>,
    /// Print resolution written into the file (pixels per inch).
    pub ppi: u16,
    /// JPEG only: largest quality whose file fits in this many KB.
    pub limit_kb: Option<u32>,
    pub sharpen: SharpenFor,
    pub sharpen_amount: SharpenAmount,
    /// File name template with the batch-rename tokens ([`crate::rename::expand_tokens`]): `{name}`
    /// (original stem), `{seq}` (zero-padded to 3, counting from `start_number`; `{seq:N}` for N
    /// digits), `{date}` (capture date, YYYYMMDD; `{date:%Y-%m-%d}`…), `{num}`, `{folder}`,
    /// `{camera}`, `{lens}`, `{iso}`, `{rating}`, `{title}`, `{creator}`, `{ext}`; the output
    /// extension is appended.
    pub naming: String,
    /// First `{seq}` value.
    pub start_number: u32,
    /// A specific folder for the whole batch, or each photo's own folder.
    pub destination_mode: DestinationMode,
    /// Created inside the destination folder (empty = none).
    pub subfolder: String,
    /// When a file of the same name exists in the destination folder.
    pub conflict: Conflict,
    /// TIFF compression.
    pub tiff_compression: TiffCompression,
    /// DNG export: how the raw data is stored.
    pub dng_compression: DngCompression,
    pub metadata: MetadataPolicy,
    /// Strip GPS / location even when the policy would include it.
    pub remove_location: bool,
    /// Text watermark (none when absent or the text is empty).
    pub watermark: Option<Watermark>,
    /// Output colour space (AVIF is always sRGB: its muxer cannot embed a profile; lossy JPEG XL
    /// writes Adobe RGB and ProPhoto as Rec. 2020, see [`ExportOptions::effective_space`]).
    pub color_space: OutputSpace,
    /// Bits per channel: 8, 16 (PNG, TIFF, JPEG XL), 32 (TIFF: float, linear) or 10 (AVIF); `None` = the
    /// format's default (TIFF 16, everything else 8). See [`ExportOptions::effective_depth`].
    pub bit_depth: Option<u8>,
    /// HDR output for photos edited in HDR: JPEG as an ISO 21496-1 gain map JPEG (the SDR
    /// rendition plus a gain map, so the file looks right everywhere; `limit_kb` includes both
    /// images and their metadata), AVIF as 10-bit Rec. 2020 PQ, PNG as 16-bit Rec. 2020 PQ (`cICP`; viewers
    /// without cICP support show it as sRGB, washed out), JPEG XL as 16-bit Rec. 2020 PQ, 32-bit float
    /// TIFF with the highlights above SDR white kept. Other formats, and photos without an HDR
    /// edit, are SDR.
    pub hdr: bool,
    /// JPEG XL: mathematically lossless instead of lossy (`quality` is then unused).
    pub jxl_lossless: bool,
    /// JPEG XL: encoder effort (time spent for a smaller file).
    pub jxl_effort: JxlEffort,
    /// JPEG XL: progressive file (a coarse image first, refined as it loads).
    pub jxl_progressive: bool,
    /// After the export, add the files written to Apple Photos (macOS only; see [`AfterExport`]).
    pub add_to_photos: bool,
    /// The Photos album they go into, made when missing (empty = none).
    pub photos_album: String,
    /// The curve of an HDR AVIF: PQ (default) or HLG.
    pub hdr_transfer: HdrCurve,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            quality: 90,
            resize: None,
            ppi: 240,
            limit_kb: None,
            sharpen: SharpenFor::None,
            sharpen_amount: SharpenAmount::Standard,
            naming: "{name}".into(),
            start_number: 1,
            destination_mode: DestinationMode::SpecificFolder,
            subfolder: String::new(),
            conflict: Conflict::Unique,
            tiff_compression: TiffCompression::Deflate,
            dng_compression: DngCompression::Lossless,
            metadata: MetadataPolicy::All,
            remove_location: false,
            watermark: None,
            color_space: OutputSpace::Srgb,
            bit_depth: None,
            hdr: false,
            jxl_lossless: false,
            jxl_effort: JxlEffort::Normal,
            jxl_progressive: false,
            add_to_photos: false,
            photos_album: String::new(),
            hdr_transfer: HdrCurve::Pq,
        }
    }
}

/// The `app.export` params that are not export options: which photos, where to, a preset to start
/// from, and the desktop app's background flag.
pub const TARGET_PARAMS: &[&str] = &["id", "ids", "path", "dir", "preset", "background"];

/// The export option params [`ExportOptions::from_json`] reads (and [`ExportOptions::to_json`] writes).
pub const OPTION_PARAMS: &[&str] = &[
    "format",
    "quality",
    "resize",
    "longEdge",
    "shortEdge",
    "width",
    "height",
    "megapixels",
    "percent",
    "fit",
    "dontEnlarge",
    "ppi",
    "limitKb",
    "sharpen",
    "sharpenAmount",
    "naming",
    "startNumber",
    "destinationMode",
    "subfolder",
    "conflict",
    "tiffCompression",
    "dngCompression",
    "metadata",
    "removeLocation",
    "watermark",
    "colorSpace",
    "bitDepth",
    "hdr",
    "jxlLossless",
    "jxlEffort",
    "jxlProgressive",
    "addToPhotos",
    "photosAlbum",
    "hdrTransfer",
];

/// Why an export can't add its files to Apple Photos on this platform (`None`: it can, when the
/// session has the runner; see [`crate::Session::after_export`]).
pub fn apple_photos_unsupported() -> Option<&'static str> {
    (!cfg!(target_os = "macos")).then_some("Apple Photos is only available on macOS")
}

/// The keys of a `watermark` object ([`Watermark`], camelCase). `inset` sets `insetX` and `insetY`
/// together (what versions before the two margins wrote); either of those, when given, wins.
pub const WATERMARK_PARAMS: &[&str] =
    &["text", "vertical", "size", "opacity", "anchor", "insetX", "insetY", "inset", "rotation", "color", "shadow", "image", "imageWidth"];

/// Text height as a fraction of the short edge: the range the renderer draws without clamping.
pub const WATERMARK_SIZE_RANGE: (f32, f32) = (0.005, 0.5);
/// Margin as a fraction of the short edge.
pub const WATERMARK_INSET_RANGE: (f32, f32) = (0.0, 0.4);
/// A graphic's width as a fraction of the photo's width.
pub const WATERMARK_IMAGE_WIDTH_RANGE: (f32, f32) = (0.01, 1.0);

/// Levenshtein distance, for "did you mean" on a misspelled key (both inputs are short).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            if let Some(c) = cur.get_mut(j + 1) {
                *c = sub.min(del).min(ins);
            }
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

/// `unknown parameter \`k\``, naming the closest known key when one is near.
fn unknown_key(what: &str, k: &str, known: &[&str]) -> String {
    let near = known.iter().map(|n| (edit_distance(&k.to_ascii_lowercase(), &n.to_ascii_lowercase()), *n)).filter(|(d, _)| *d <= 2).min();
    match near {
        Some((_, n)) => format!("unknown {what} `{k}` (did you mean `{n}`?)"),
        None => format!("unknown {what} `{k}` (one of {})", known.join(", ")),
    }
}

/// A JSON integer (`92`; never `92.5` or `"92"`), as [`ExportOptions::from_json`] reads them.
fn whole(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
}

/// A `watermark` object as a [`Watermark`]; a legacy `inset` fills whichever of `insetX` and
/// `insetY` is missing.
fn watermark_from_json(v: &serde_json::Value) -> serde_json::Result<Watermark> {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut()
        && let Some(inset) = o.remove("inset")
    {
        o.entry("insetX").or_insert_with(|| inset.clone());
        o.entry("insetY").or_insert(inset);
    }
    serde_json::from_value(v)
}

impl ExportOptions {
    /// Stored export settings (a saved preset, `prefs.json ▸ lastExport`) with the keys an older or
    /// newer version may have written and this one doesn't know dropped, also inside `watermark`.
    /// [`ExportOptions::validate`] is strict about what a caller types; settings saved on disk are
    /// not the caller's typo, and refusing them would make a preset or Export with Previous fail
    /// for good after an update. A `bitDepth` the stored format doesn't have (written at the
    /// format's own depth before issue #1952) is dropped too.
    pub fn known_keys_only(p: &serde_json::Value) -> serde_json::Value {
        let mut p = Self::known_keys(p);
        Self::drop_unfit_depth(&mut p, ExportFormat::default());
        p
    }

    /// [`ExportOptions::known_keys_only`] keeping `bitDepth`, for a preset whose format may only be
    /// known once the call's own params (`path`) are merged in.
    pub(crate) fn known_keys(p: &serde_json::Value) -> serde_json::Value {
        let mut p = p.clone();
        if let Some(o) = p.as_object_mut() {
            o.retain(|k, _| OPTION_PARAMS.contains(&k.as_str()) || TARGET_PARAMS.contains(&k.as_str()));
            if let Some(w) = o.get_mut("watermark").and_then(serde_json::Value::as_object_mut) {
                w.retain(|k, _| WATERMARK_PARAMS.contains(&k.as_str()));
            }
        }
        p
    }

    /// Drop a stored `bitDepth` the format `p` exports ([`ExportOptions::export_format`] with
    /// `fallback`) doesn't have: it was written at the format's own depth before issue #1952.
    pub(crate) fn drop_unfit_depth(p: &mut serde_json::Value, fallback: ExportFormat) {
        let format = Self::export_format(p, fallback);
        if let Some(o) = p.as_object_mut()
            && let Some(depth) = o.get("bitDepth").and_then(whole)
            && !Self::bit_depths(format).iter().any(|d| i64::from(d.0) == depth)
        {
            o.remove("bitDepth");
        }
    }

    /// Strict [`ExportOptions::from_json`] for a command that is about to export (issue #181): an
    /// unknown parameter or a value that cannot be read as what it is for is an error, never a
    /// silent default. `app.export` targets (`id`, `ids`, `path`, `dir`, `preset`, `background`)
    /// are accepted and left to the caller.
    pub fn from_params(p: &serde_json::Value) -> crate::Result<Self> {
        Self::from_params_or(p, ExportFormat::default())
    }

    /// [`ExportOptions::from_params`] for a caller that writes `fallback` to a `path` naming no
    /// format; the format returned is [`ExportOptions::export_format`], the one written.
    pub fn from_params_or(p: &serde_json::Value, fallback: ExportFormat) -> crate::Result<Self> {
        let format = Self::export_format(p, fallback);
        Self::validate_as("app.export", p, Some(format))?;
        Ok(Self { format, ..Self::from_json(p) })
    }

    /// The format `app.export` writes: `format` (`null`: the default), else the one the extension of
    /// `path` names, else `fallback` for a path that names none (the desktop app writes JPEG there,
    /// the headless backend PNG), else the default.
    pub fn export_format(p: &serde_json::Value, fallback: ExportFormat) -> ExportFormat {
        use serde_json::Value;
        match (p.get("format"), p.get("path").and_then(Value::as_str)) {
            (Some(f), _) => f.as_str().and_then(ExportFormat::parse).unwrap_or_default(),
            (None, Some(path)) => std::path::Path::new(path).extension().and_then(|e| e.to_str()).and_then(ExportFormat::parse).unwrap_or(fallback),
            (None, None) => ExportFormat::default(),
        }
    }

    /// Check `app.export` params for `cmd` (the command named in the error): every key must be an
    /// export option ([`OPTION_PARAMS`]) or a target ([`TARGET_PARAMS`]), every value must be the
    /// kind of thing its key takes, and a `watermark` object must hold only [`WATERMARK_PARAMS`]
    /// with its sizes in range (issue #183). `null` means "not given", as a dropped key does.
    pub fn validate(cmd: &str, p: &serde_json::Value) -> crate::Result<()> {
        Self::validate_as(cmd, p, Some(Self::export_format(p, ExportFormat::default())))
    }

    /// [`ExportOptions::validate`] with `bitDepth` checked against `format`, the one exported
    /// (`None`: not known yet, as for a preset without `format`).
    pub(crate) fn validate_as(cmd: &str, p: &serde_json::Value, format: Option<ExportFormat>) -> crate::Result<()> {
        use serde_json::Value;
        let bad = |msg: String| crate::cmd::bad(cmd, msg);
        let Some(obj) = p.as_object() else { return Err(bad("params must be an object".into())) };
        let known: Vec<&str> = OPTION_PARAMS.iter().chain(TARGET_PARAMS).copied().collect();
        let int = |k: &str, v: &Value, lo: i64, hi: i64| -> crate::Result<i64> {
            whole(v).filter(|i| (lo..=hi).contains(i)).ok_or_else(|| bad(format!("`{k}` must be an integer {lo}..{hi}")))
        };
        let num = |k: &str, v: &Value, lo: f64, hi: f64| -> crate::Result<f64> {
            v.as_f64().filter(|f| f.is_finite() && (lo..=hi).contains(f)).ok_or_else(|| bad(format!("`{k}` must be a number {lo}..{hi}")))
        };
        fn string<'a>(cmd: &str, k: &str, v: &'a Value) -> crate::Result<&'a str> {
            v.as_str().ok_or_else(|| crate::cmd::bad(cmd, format!("`{k}` must be a string")))
        }
        let boolean = |k: &str, v: &Value| -> crate::Result<bool> { v.as_bool().ok_or_else(|| bad(format!("`{k}` must be true or false"))) };
        let one_of = |k: &str, v: &Value, options: &[&str]| -> crate::Result<()> {
            let s = string(cmd, k, v)?;
            if options.iter().any(|o| o.eq_ignore_ascii_case(s)) { Ok(()) } else { Err(bad(format!("`{k}` must be one of {}", options.join("|")))) }
        };
        // a choice `from_json` reads with the same `parse`, so what passes here is what gets exported
        let parses = |k: &str, v: &Value, parse: fn(&str) -> bool, options: &[&str]| -> crate::Result<()> {
            if parse(string(cmd, k, v)?) { Ok(()) } else { Err(bad(format!("`{k}` must be one of {}", options.join("|")))) }
        };
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            match k.as_str() {
                "format" => {
                    if ExportFormat::parse(string(cmd, k, v)?).is_none() {
                        return Err(bad(format!("`{k}` must be one of jpeg|png|tiff|webp|avif|jxl|dng|original")));
                    }
                }
                "quality" => {
                    int(k, v, 1, 100)?;
                }
                "ppi" => {
                    int(k, v, 1, u16::MAX as i64)?;
                }
                "limitKb" => {
                    int(k, v, 0, u32::MAX as i64)?;
                }
                "startNumber" => {
                    int(k, v, 0, 999_999)?;
                }
                "bitDepth" => {
                    if !matches!(whole(v), Some(8 | 10 | 16 | 32)) {
                        return Err(bad(format!("`{k}` must be 8, 10, 16 or 32")));
                    }
                }
                "longEdge" | "shortEdge" | "width" | "height" | "megapixels" | "percent" => {
                    num(k, v, 0.0, 1.0e9)?;
                }
                "fit" => one_of(k, v, &["dimensions", "widthHeight"])?,
                "resize" => {
                    let Some(r) = v.as_object() else { return Err(bad(format!("`{k}` must be an object {{mode, value, height?, dontEnlarge?}}"))) };
                    if let Some(x) = r.keys().find(|x| !matches!(x.as_str(), "mode" | "value" | "height" | "dontEnlarge")) {
                        return Err(bad(unknown_key("resize key", x, &["mode", "value", "height", "dontEnlarge"])));
                    }
                    serde_json::from_value::<Resize>(v.clone()).map_err(|e| bad(format!("`{k}`: {e}")))?;
                }
                "dontEnlarge" | "removeLocation" | "background" | "hdr" | "jxlLossless" | "jxlProgressive" => {
                    boolean(k, v)?;
                }
                "addToPhotos" => {
                    if boolean(k, v)?
                        && let Some(why) = apple_photos_unsupported()
                    {
                        return Err(bad(format!("`{k}`: {why}")));
                    }
                }
                "photosAlbum" => {
                    if string(cmd, k, v)?.contains('\0') {
                        return Err(bad(format!("`{k}` can't contain a NUL character")));
                    }
                }
                "naming" | "subfolder" | "path" | "dir" | "preset" => {
                    string(cmd, k, v)?;
                }
                "sharpen" => parses(k, v, |s| SharpenFor::parse(s).is_some(), &["none", "screen", "matte", "glossy"])?,
                "sharpenAmount" => parses(k, v, |s| SharpenAmount::parse(s).is_some(), &["low", "standard", "high"])?,
                "hdrTransfer" => one_of(k, v, &["pq", "hlg"])?,
                "conflict" => parses(k, v, |s| Conflict::parse(s).is_some(), &["unique", "overwrite", "skip"])?,
                "destinationMode" => one_of(k, v, &["specificFolder", "sameAsOriginal"])?,
                "tiffCompression" => one_of(k, v, &["none", "lzw", "zip", "deflate"])?,
                "jxlEffort" => one_of(k, v, &["fast", "normal"])?,
                "dngCompression" => {
                    serde_json::from_value::<DngCompression>(v.clone())
                        .map_err(|_| bad(format!("`{k}` must be one of lossless|deflate|uncompressed|lossy")))?;
                }
                "metadata" => parses(k, v, |s| MetadataPolicy::parse(s).is_some(), &["all", "allExceptCamera", "copyright", "none"])?,
                "colorSpace" => {
                    if OutputSpace::parse(string(cmd, k, v)?).is_none() {
                        return Err(bad(format!("`{k}` must be one of srgb|displayP3|adobeRgb|proPhoto|rec2020")));
                    }
                }
                "watermark" => Self::validate_watermark(cmd, v)?,
                "id" => {
                    int(k, v, 0, i64::MAX)?;
                }
                "ids" => {
                    let ok = v.as_array().is_some_and(|a| a.iter().all(|x| x.is_u64()));
                    if !ok {
                        return Err(bad(format!("`{k}` must be an array of photo ids")));
                    }
                }
                _ => return Err(bad(unknown_key("parameter", k, &known))),
            }
        }
        // issue #1952: a depth the format doesn't have would be written at the format's own depth
        if let (Some(format), Some(depth)) = (format, obj.get("bitDepth").and_then(whole)) {
            let mut depths: Vec<u8> = Self::bit_depths(format).iter().map(|d| d.0).collect();
            if !depths.iter().any(|d| i64::from(*d) == depth) {
                depths.sort_unstable();
                let mut list: Vec<String> = depths.iter().map(u8::to_string).collect();
                let last = list.pop().unwrap_or_default();
                let list = if list.is_empty() { last } else { format!("{} or {last}", list.join(", ")) };
                let name = serde_json::to_value(format).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                return Err(bad(format!("`bitDepth` must be {list} for {name}")));
            }
        }
        Ok(())
    }

    /// A `watermark` value: text, or an object of [`WATERMARK_PARAMS`] (issue #183).
    pub(crate) fn validate_watermark(cmd: &str, v: &serde_json::Value) -> crate::Result<()> {
        use serde_json::Value;
        let bad = |msg: String| crate::cmd::bad(cmd, msg);
        let obj = match v {
            Value::String(_) => return Ok(()),
            Value::Object(o) => o,
            _ => {
                return Err(bad(
                    "`watermark` must be text or an object {text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth}".into(),
                ));
            }
        };
        let frac = |k: &str, v: &Value, (lo, hi): (f32, f32), unit: &str| -> crate::Result<()> {
            match v.as_f64() {
                Some(f) if f.is_finite() && (f64::from(lo)..=f64::from(hi)).contains(&f) => Ok(()),
                _ => Err(bad(format!("`watermark.{k}` must be a number {lo}..{hi} ({unit})"))),
            }
        };
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            match k.as_str() {
                "text" | "image" => {
                    v.as_str().ok_or_else(|| bad(format!("`watermark.{k}` must be a string")))?;
                }
                "vertical" | "shadow" => {
                    v.as_bool().ok_or_else(|| bad(format!("`watermark.{k}` must be true or false")))?;
                }
                "size" => frac(k, v, WATERMARK_SIZE_RANGE, "fraction of the short edge")?,
                "opacity" => frac(k, v, (0.0, 1.0), "0 = invisible, 1 = solid")?,
                "insetX" | "insetY" | "inset" => frac(k, v, WATERMARK_INSET_RANGE, "fraction of the short edge")?,
                "imageWidth" => frac(k, v, WATERMARK_IMAGE_WIDTH_RANGE, "fraction of the photo's width")?,
                "rotation" => {
                    if !matches!(whole(v), Some(0 | 90 | 180 | 270)) {
                        return Err(bad("`watermark.rotation` must be 0, 90, 180 or 270 (degrees clockwise)".into()));
                    }
                }
                "anchor" => {
                    serde_json::from_value::<Anchor>(v.clone()).map_err(|_| {
                        bad("`watermark.anchor` must be one of topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight".into())
                    })?;
                }
                "color" => {
                    let ok = v.as_array().is_some_and(|a| a.len() == 3 && a.iter().all(|c| matches!(whole(c), Some(0..=255))));
                    if !ok {
                        return Err(bad("`watermark.color` must be [r, g, b] with 0..255 each (sRGB)".into()));
                    }
                }
                _ => return Err(bad(unknown_key("watermark key", k, WATERMARK_PARAMS))),
            }
        }
        watermark_from_json(v).map(|_| ()).map_err(|e| bad(format!("`watermark`: {e}")))
    }

    /// Read options from command params (`format`, `quality`, `limitKb`, `sharpen`, `sharpenAmount`,
    /// `naming`, `metadata`, `removeLocation`, `watermark`, `colorSpace`, `bitDepth`, `ppi`,
    /// `addToPhotos`, `photosAlbum`) and the
    /// size: one of `longEdge`, `shortEdge`, `width`, `height` (both = fit inside W × H),
    /// `megapixels`, `percent` (absent or 0 = full size), or a [`Resize`] object as `resize`; plus
    /// `dontEnlarge` (default true).
    ///
    /// Lenient: anything unknown or unreadable falls back to the default, which suits stored
    /// prefs and dialog prefill. A command that is about to export uses
    /// [`ExportOptions::from_params`], which validates first.
    pub fn from_json(p: &serde_json::Value) -> Self {
        use serde_json::Value;
        let d = Self::default();
        let s = |k: &str| p.get(k).and_then(Value::as_str);
        let u = |k: &str| p.get(k).and_then(Value::as_u64);
        fn enm<T: serde::de::DeserializeOwned>(p: &Value, k: &str) -> Option<T> {
            p.get(k).cloned().and_then(|v| serde_json::from_value(v).ok())
        }
        Self {
            format: s("format").and_then(ExportFormat::parse).unwrap_or(d.format),
            quality: u("quality").map_or(d.quality, |q| q.clamp(1, 100) as u8),
            resize: Self::resize_from_json(p),
            ppi: u("ppi").filter(|v| *v > 0).map_or(d.ppi, |v| v.min(u16::MAX as u64) as u16),
            limit_kb: u("limitKb").filter(|v| *v > 0).map(|v| v.min(u32::MAX as u64) as u32),
            sharpen: s("sharpen").and_then(SharpenFor::parse).unwrap_or(d.sharpen),
            sharpen_amount: s("sharpenAmount").and_then(SharpenAmount::parse).unwrap_or(d.sharpen_amount),
            naming: s("naming").map_or(d.naming, str::to_string),
            start_number: u("startNumber").map_or(d.start_number, |v| v.min(999_999) as u32),
            destination_mode: s("destinationMode").and_then(DestinationMode::parse).unwrap_or(d.destination_mode),
            subfolder: s("subfolder").map_or(d.subfolder, |v| v.trim().replace(['\\', ':', '\0'], "_")),
            conflict: s("conflict").and_then(Conflict::parse).unwrap_or(d.conflict),
            tiff_compression: match s("tiffCompression").map(str::to_ascii_lowercase).as_deref() {
                Some("none") => TiffCompression::None,
                Some("lzw") => TiffCompression::Lzw,
                Some("zip" | "deflate") => TiffCompression::Deflate,
                _ => d.tiff_compression,
            },
            dng_compression: enm(p, "dngCompression").unwrap_or(d.dng_compression),
            metadata: s("metadata").and_then(MetadataPolicy::parse).unwrap_or(d.metadata),
            remove_location: p.get("removeLocation").and_then(Value::as_bool).unwrap_or(d.remove_location),
            watermark: match p.get("watermark") {
                Some(Value::String(t)) => Some(Watermark { text: t.clone(), ..Default::default() }),
                Some(v @ Value::Object(_)) => watermark_from_json(v).ok(),
                _ => None,
            }
            .filter(|w: &Watermark| !w.text.trim().is_empty() || !w.image.trim().is_empty()),
            color_space: s("colorSpace").and_then(OutputSpace::parse).unwrap_or(d.color_space),
            bit_depth: u("bitDepth").filter(|b| matches!(b, 8 | 10 | 16 | 32)).map(|b| b as u8),
            hdr: p.get("hdr").and_then(Value::as_bool).unwrap_or(d.hdr),
            jxl_lossless: p.get("jxlLossless").and_then(Value::as_bool).unwrap_or(d.jxl_lossless),
            jxl_effort: s("jxlEffort").and_then(JxlEffort::parse).unwrap_or(d.jxl_effort),
            jxl_progressive: p.get("jxlProgressive").and_then(Value::as_bool).unwrap_or(d.jxl_progressive),
            add_to_photos: p.get("addToPhotos").and_then(Value::as_bool).unwrap_or(d.add_to_photos),
            photos_album: s("photosAlbum").map_or(d.photos_album, |a| a.trim().to_string()),
            hdr_transfer: s("hdrTransfer").and_then(HdrCurve::parse).unwrap_or(d.hdr_transfer),
        }
    }

    fn resize_from_json(p: &serde_json::Value) -> Option<Resize> {
        use serde_json::Value;
        let f = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| *v > 0.0);
        let dont_enlarge = p.get("dontEnlarge").and_then(Value::as_bool).unwrap_or(true);
        if let Some(r) = p.get("resize").filter(|v| v.is_object()) {
            let r: Resize = serde_json::from_value(r.clone()).ok()?;
            return (r.value > 0.0).then_some(Resize { dont_enlarge: p.get("dontEnlarge").and_then(Value::as_bool).unwrap_or(r.dont_enlarge), ..r });
        }
        let r = |mode, value: f64| Resize { mode, value: value.min(65_535.0) as f32, height: 0, dont_enlarge };
        // `fit: widthHeight` binds each edge to its own number; the default fits either orientation
        let bound = p.get("fit").and_then(Value::as_str).is_some_and(|fit| fit.eq_ignore_ascii_case("widthHeight"));
        Some(match (f("width"), f("height")) {
            (Some(w), Some(h)) => {
                Resize { height: h.min(65_535.0) as u32, ..r(if bound { ResizeMode::WidthHeight } else { ResizeMode::Dimensions }, w) }
            }
            (Some(w), None) => r(ResizeMode::Width, w),
            (None, Some(h)) => r(ResizeMode::Height, h),
            (None, None) => {
                if let Some(v) = f("longEdge") {
                    r(ResizeMode::LongEdge, v)
                } else if let Some(v) = f("shortEdge") {
                    r(ResizeMode::ShortEdge, v)
                } else if let Some(v) = f("megapixels") {
                    r(ResizeMode::Megapixels, v)
                } else {
                    let v = f("percent")?;
                    r(ResizeMode::Percent, v)
                }
            }
        })
    }

    /// These options as `app.export` params ([`ExportOptions::from_json`] reads them back
    /// unchanged; full size is written as `longEdge: 0`).
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("resize");
            if let Some(sz) = Self::resize_json(self.resize.as_ref()).as_object() {
                o.extend(sz.clone());
            }
            if self.resize.is_none() {
                o.insert("longEdge".into(), 0.into());
            }
            o.retain(|_, v| !v.is_null());
        }
        v
    }

    /// Whether `p` names an output size (any of the size params, even 0 = full size).
    pub fn has_size_param(p: &serde_json::Value) -> bool {
        ["resize", "longEdge", "shortEdge", "width", "height", "megapixels", "percent"].iter().any(|k| p.get(k).is_some())
    }

    /// The size params of [`ExportOptions::from_json`] for `resize` (`{}` = full size).
    pub fn resize_json(resize: Option<&Resize>) -> serde_json::Value {
        match resize {
            None => serde_json::json!({}),
            Some(r) => serde_json::json!({"resize": r, "dontEnlarge": r.dont_enlarge}),
        }
    }

    /// The sample format the file is written with (unsupported requests fall back to the closest).
    pub fn effective_depth(&self) -> OutputDepth {
        match (self.format, self.bit_depth) {
            (ExportFormat::Jpeg | ExportFormat::Webp | ExportFormat::Original | ExportFormat::Dng, _) => OutputDepth::U8,
            (ExportFormat::Png, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Png, Some(16 | 32)) => OutputDepth::U16,
            (ExportFormat::Png, _) => OutputDepth::U8,
            (ExportFormat::Tiff, Some(8)) => OutputDepth::U8,
            (ExportFormat::Tiff, Some(32)) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Tiff, Some(32)) => OutputDepth::F32Linear,
            (ExportFormat::Tiff, _) => OutputDepth::U16,
            (ExportFormat::RenderedDng, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::RenderedDng, _) => OutputDepth::F32Linear,
            (ExportFormat::Psd, Some(8)) => OutputDepth::U8,
            (ExportFormat::Psd, _) => OutputDepth::U16,
            (ExportFormat::Avif, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Avif, Some(10 | 16 | 32)) => OutputDepth::U16,
            (ExportFormat::Avif, _) => OutputDepth::U8,
            (ExportFormat::Jxl, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Jxl, Some(10 | 16 | 32)) => OutputDepth::U16,
            (ExportFormat::Jxl, _) => OutputDepth::U8,
        }
    }

    /// The bit depths `format` offers (value, label); the first is its default.
    pub fn bit_depths(format: ExportFormat) -> &'static [(u8, &'static str)] {
        match format {
            ExportFormat::Jpeg | ExportFormat::Webp | ExportFormat::Original | ExportFormat::Dng => &[(8, "8-bit")],
            ExportFormat::Png => &[(8, "8-bit"), (16, "16-bit")],
            ExportFormat::Tiff => &[(16, "16-bit"), (8, "8-bit"), (32, "32-bit float")],
            ExportFormat::Psd => &[(16, "16-bit"), (8, "8-bit")],
            ExportFormat::RenderedDng => &[(16, "16-bit float")],
            ExportFormat::Avif => &[(8, "8-bit"), (10, "10-bit")],
            ExportFormat::Jxl => &[(8, "8-bit"), (16, "16-bit")],
        }
    }

    /// Whether these options write HDR files (for photos edited in HDR): JPEG (gain map), AVIF,
    /// PNG or JPEG XL (PQ) or 32-bit float TIFF with [`ExportOptions::hdr`].
    pub fn hdr_output(&self) -> bool {
        self.hdr
            && matches!(
                (self.format, self.bit_depth),
                (ExportFormat::Jpeg | ExportFormat::Avif | ExportFormat::Png | ExportFormat::Jxl, _) | (ExportFormat::Tiff, Some(32))
            )
    }

    /// The colour space the file is actually written in (AVIF: sRGB; AVIF, PNG and JPEG XL HDR:
    /// Rec. 2020; lossy JPEG XL: Adobe RGB and ProPhoto as Rec. 2020, the widest space its encoder
    /// can declare without a profile).
    pub fn effective_space(&self) -> OutputSpace {
        match self.format {
            ExportFormat::Avif | ExportFormat::Png | ExportFormat::Jxl if self.hdr => OutputSpace::Rec2020,
            ExportFormat::Avif => OutputSpace::Srgb,
            ExportFormat::Jxl if !self.jxl_lossless && matches!(self.color_space, OutputSpace::AdobeRgb | OutputSpace::ProPhoto) => {
                OutputSpace::Rec2020
            }
            _ => self.color_space,
        }
    }

    /// The JPEG XL encoder settings for these options, writing samples in `space`.
    fn jxl(&self, space: OutputSpace, hdr: bool) -> JxlOptions {
        let colour = match space {
            _ if hdr => JxlColour::Rec2020Pq,
            OutputSpace::DisplayP3 => JxlColour::DisplayP3,
            OutputSpace::Rec2020 => JxlColour::Rec2020,
            // lossless files carry the ICC profile; lossy ones never get here (`effective_space`)
            OutputSpace::Srgb | OutputSpace::AdobeRgb | OutputSpace::ProPhoto => JxlColour::Srgb,
        };
        JxlOptions { quality: self.quality, lossless: self.jxl_lossless, effort: self.jxl_effort, progressive: self.jxl_progressive, colour }
    }

    /// Output file name for photo `p` at 1-based position `seq` in a batch (the original's
    /// extension is kept for [`ExportFormat::Original`]).
    pub fn file_name_for(&self, p: &lightcraft_catalog::Photo, seq: usize) -> String {
        let base = if self.naming.trim().is_empty() { "{name}" } else { self.naming.as_str() };
        let n = seq + self.start_number.max(1) as usize - 1;
        let name = crate::rename::expand_tokens(base, p, n, 3);
        let mut name: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).collect();
        let (stem, ext) = p.file_name.rsplit_once('.').unwrap_or((&p.file_name, ""));
        if name.trim().is_empty() {
            // every token came out empty (no title, no capture time…): keep the original name
            name = stem.to_string();
        }
        if self.format == ExportFormat::Original {
            let base = name.trim_end_matches('.');
            if ext.is_empty() { base.to_string() } else { format!("{base}.{ext}") }
        } else {
            format!("{name}.{}", self.format.extension())
        }
    }
}

/// Unsharp mask on an 8-bit image (separable box-blur approximation of a small Gaussian).
pub fn output_sharpen(img: &mut Rgba8, target: SharpenFor, amount: SharpenAmount) {
    let src: Vec<[f32; 3]> = img.data.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if let Some(out) = unsharp(img.width, img.height, &src, target, amount) {
        for (p, v) in img.data.iter_mut().zip(out) {
            for c in 0..3 {
                p[c] = v[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// [`output_sharpen`] on a high-bit-depth image (float images are sharpened in linear light).
pub fn output_sharpen_deep(img: &mut DeepImage, target: SharpenFor, amount: SharpenAmount) {
    let (w, h) = (img.width, img.height);
    match &mut img.samples {
        DeepSamples::U16(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
        DeepSamples::F32(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.max(0.0);
                }
            }
        }
    }
}

/// The sharpened values of `src` (`w × h`), or `None` when nothing is to be done.
fn unsharp(w: usize, h: usize, src: &[[f32; 3]], target: SharpenFor, amount: SharpenAmount) -> Option<Vec<[f32; 3]>> {
    let (radius, base) = match target {
        SharpenFor::None => return None,
        SharpenFor::Screen => (1usize, 0.35f32),
        SharpenFor::Matte => (2, 0.6),
        SharpenFor::Glossy => (1, 0.5),
    };
    let k = base
        * match amount {
            SharpenAmount::Low => 0.6,
            SharpenAmount::Standard => 1.0,
            SharpenAmount::High => 1.5,
        };
    if w < 3 || h < 3 {
        return None;
    }
    let r = radius as isize;
    let box_pass = |inp: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; inp.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 3];
                for d in -r..=r {
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    let p = inp[sy * w + sx];
                    acc = [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                out[y * w + x] = acc.map(|v| v / n);
            }
        }
        out
    };
    let blur = box_pass(&box_pass(src, true), false);
    Some(src.iter().zip(&blur).map(|(s, b)| std::array::from_fn(|c| s[c] + (s[c] - b[c]) * k)).collect())
}

/// Encode a rendered display-referred image according to `o` (its pixels must already be in
/// `o.effective_space()`). Resizing to `long_edge` is the
/// caller's job (render at that size); sharpening is applied here.
pub fn encode_image(img: &Rgba8, o: &ExportOptions) -> Result<Vec<u8>, String> {
    encode_with_metadata(img, o, None)
}

/// Like [`encode_image`], embedding `meta` (already filtered by the policy) as EXIF + XMP.
pub fn encode_with_metadata(img: &Rgba8, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let img = finish_8bit(img, o);
    let space = o.effective_space();
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    // Opaque photo exports need no alpha channel (#1670). Keep real transparency for
    // callers of this encoder, and leave unrelated formats on their existing path.
    let rgb: Option<Vec<u8>> = (matches!(o.format, ExportFormat::Png | ExportFormat::Tiff | ExportFormat::Webp)
        && img.data.iter().all(|p| p.last() == Some(&255)))
    .then(|| img.data.iter().flat_map(|p| p.iter().take(3).copied()).collect());
    let e = match &rgb {
        Some(data) => EncodeImage::new(img.width as u32, img.height as u32, 3, Samples::U8(data)),
        None => EncodeImage::rgba8(&img),
    };
    let r = match o.format {
        ExportFormat::Jpeg => {
            // 4:4:4 normally; 4:2:0 when targeting a file size (much smaller at equal visual quality).
            let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
            return encode_jpeg_with_limit(o, |q| encode::encode_jpeg(&e, q, sub, &meta));
        }
        ExportFormat::Png => encode::encode_png(&e, &meta),
        ExportFormat::Tiff => encode::encode_tiff(&e, o.tiff_compression, &meta),
        ExportFormat::Psd => encode::encode_psd(&e, &meta),
        ExportFormat::Webp => encode::encode_webp_lossless(&e, &meta),
        ExportFormat::Avif => encode::encode_avif(&e, o.quality, 8, &meta),
        ExportFormat::Jxl => lightcraft_codecs::encode_jxl(&e, &o.jxl(space, false), &meta),
        f @ (ExportFormat::Original | ExportFormat::Dng) => return Err(format!("{f:?} export does not encode pixels")),
        ExportFormat::RenderedDng => return Err("Render to DNG needs a float render".into()),
    };
    r.map_err(|e| e.to_string())
}

/// Apply the same file-size policy to ordinary JPEGs and complete HDR gain map JPEGs.
fn encode_jpeg_with_limit(o: &ExportOptions, jpeg: impl Fn(u8) -> lightcraft_codecs::Result<Vec<u8>>) -> Result<Vec<u8>, String> {
    let Some(kb) = o.limit_kb else { return jpeg(o.quality).map_err(|e| e.to_string()) };
    let limit = u64::from(kb) * 1024;
    let (mut lo, mut hi) = (1u8, o.quality.min(100));
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let b = jpeg(mid).map_err(|e| e.to_string())?;
        if b.len() as u64 <= limit {
            best = Some(b);
            lo = mid + 1;
        } else if mid == 1 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    best.ok_or_else(|| format!("cannot fit within {kb} KB"))
}

/// Output sharpening and the watermark on an 8-bit render in `o.effective_space()`.
fn finish_8bit(img: &Rgba8, o: &ExportOptions) -> Rgba8 {
    let mut img = img.clone();
    output_sharpen(&mut img, o.sharpen, o.sharpen_amount);
    let space = o.effective_space();
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(space, wm.color), target: Some(space), ..wm.clone() };
        draw_watermark(&mut img, &wm);
    }
    img
}

/// Output sharpening and the watermark on a high-bit-depth render.
fn finish_deep(img: &DeepImage, o: &ExportOptions) -> DeepImage {
    let mut img = img.clone();
    output_sharpen_deep(&mut img, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(img.space, wm.color), target: Some(img.space), ..wm.clone() };
        draw_watermark_deep(&mut img, &wm);
    }
    img
}

/// Encode an ISO 21496-1 gain map JPEG from the SDR rendition `sdr` (8-bit, in the output space)
/// and the HDR rendition `hdr` (float linear, same space and size). The gain map is measured
/// against the base as written, so its 8-bit rounding is handed back by the map.
pub fn encode_gain_map_jpeg(sdr: &Rgba8, hdr: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    use lightcraft_codecs::gainmap;
    let base = finish_8bit(sdr, o);
    let hdr = finish_deep(hdr, o);
    let DeepSamples::F32(hv) = &hdr.samples else {
        return Err("HDR export needs a float HDR render".into());
    };
    let (w, h) = (base.width, base.height);
    if (hdr.width, hdr.height) != (w, h) || hv.len() < w * h * 3 {
        return Err("HDR and SDR renders differ in size".into());
    }
    let space = o.effective_space();
    let trc = space.trc();
    let sdr_lin: Vec<[f32; 3]> = base.data.iter().map(|p| [0, 1, 2].map(|c| trc.decode(p[c] as f32 / 255.0))).collect();
    let hdr_lin: &[[f32; 3]] = hv.as_chunks::<3>().0;
    let (map, gm) = gainmap::compute(&sdr_lin, hdr_lin, w, h, space.luma(), &gainmap::GainMapOptions::default()).map_err(|e| e.to_string())?;
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let em = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
    encode_jpeg_with_limit(o, |q| gainmap::encode_jpeg(&EncodeImage::rgba8(&base), &map, &gm, q, sub, &em))
}

/// The codecs' name of an output space (for its ICC profile).
pub fn named_space(s: OutputSpace) -> NamedSpace {
    match s {
        OutputSpace::Srgb => NamedSpace::Srgb,
        OutputSpace::DisplayP3 => NamedSpace::DisplayP3,
        OutputSpace::AdobeRgb => NamedSpace::AdobeRgb,
        OutputSpace::ProPhoto => NamedSpace::ProPhoto,
        OutputSpace::Rec2020 => NamedSpace::Rec2020,
    }
}

/// An 8-bit sRGB colour expressed in `space` (encoded with its curve).
pub fn srgb8_in(space: OutputSpace, c: [u8; 3]) -> [u8; 3] {
    if space == OutputSpace::Srgb {
        return c;
    }
    let lin = c.map(lightcraft_color::transfer::decode_srgb8);
    let m = lightcraft_color::SRGB.to_space(&space.rgb_space()).to_f32();
    let t = space.trc();
    std::array::from_fn(|i| {
        let v = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
        (t.encode(v) * 255.0 + 0.5) as u8
    })
}

/// Encode a render according to `o`: its high-bit-depth samples when it has them (16-bit PNG/TIFF,
/// 10-bit AVIF, 32-bit float linear TIFF), else its 8-bit image.
pub fn encode_rendered(r: &lightcraft_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    match &r.deep {
        Some(d) if o.effective_depth() != OutputDepth::U8 => encode_deep(d, o, meta),
        _ => encode_with_metadata(&r.image, o, meta),
    }
}

/// Encode a high-bit-depth image (see [`encode_rendered`]).
pub fn encode_deep(img: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let meta_src = meta;
    let img = finish_deep(img, o);
    let profile = match img.samples {
        DeepSamples::F32(_) => icc::write_matrix_trc(&img.space.rgb_space(), &lightcraft_codecs::Trc::Linear),
        DeepSamples::U16(_) => icc::write_named(named_space(img.space)),
    };
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let (w, h) = (img.width as u32, img.height as u32);
    let e = match &img.samples {
        DeepSamples::U16(v) => EncodeImage::new(w, h, 3, Samples::U16(v)),
        DeepSamples::F32(v) => EncodeImage::new(w, h, 3, Samples::F32(v)),
    };
    let r = match (o.format, &img.samples) {
        (ExportFormat::Png, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => lightcraft_codecs::encode_png_pq(w, h, v, &meta),
        (ExportFormat::Png, _) => encode::encode_png(&e, &meta),
        (ExportFormat::Tiff, _) => encode::encode_tiff(&e, o.tiff_compression, &meta),
        (ExportFormat::Psd, _) => encode::encode_psd(&e, &meta),
        (ExportFormat::RenderedDng, DeepSamples::F32(v)) => {
            let m = meta_src.cloned().unwrap_or_default();
            return lightcraft_raw::write_rendered_dng(img.width, img.height, v, &img.space.rgb_space(), &m, xmp.clone()).map_err(|e| e.to_string());
        }
        (ExportFormat::Avif, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_avif_hdr(w, h, v, o.quality, 8, &meta, o.hdr_transfer.codec())
        }
        (ExportFormat::Avif, _) => encode::encode_avif(&e, o.quality, 8, &meta),
        (ExportFormat::Jxl, DeepSamples::F32(_)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, true), &meta)
        }
        (ExportFormat::Jxl, DeepSamples::U16(_)) => lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, false), &meta),
        (f, _) => return Err(format!("{f:?} export is 8-bit only")),
    };
    r.map_err(|e| e.to_string())
}

/// Whether the export of `photo` under `o` leaves out its location (GPS and place names).
/// (`photo` and `catalog` let library-level privacy rules add to the export option.)
pub fn removes_location(_photo: &lightcraft_catalog::Photo, _catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> bool {
    o.remove_location
}

/// The metadata to embed for `photo` under `o.metadata` / `o.remove_location`. `None` = embed nothing.
/// Keywords follow the catalog's keyword list (`Catalog::export_keywords`).
pub fn export_metadata(photo: &lightcraft_catalog::Photo, catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> Option<Metadata> {
    let m = &photo.meta;
    let text = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // copyright info (also under "copyright only"): notice, creator, status, usage terms, info URL
    let mut out = Metadata {
        copyright: text(&m.copyright),
        copyright_marked: m.copyright_status.marked(),
        usage_terms: text(&m.usage_terms),
        copyright_url: text(&m.copyright_url),
        artist: text(&m.creator),
        software: Some("LightCraft".into()),
        ..Default::default()
    };
    match o.metadata {
        MetadataPolicy::None => return None,
        MetadataPolicy::Copyright => return Some(out),
        MetadataPolicy::All | MetadataPolicy::AllExceptCamera => {}
    }
    out.title = text(&m.title);
    out.caption = text(&m.caption);
    out.alt_text = text(&m.alt_text);
    out.extended_description = text(&m.extended_description);
    if !o.remove_location {
        out.sublocation = text(&m.location);
        out.city = text(&m.city);
        out.state = text(&m.state);
        out.country = text(&m.country);
    }
    let keywords = catalog.export_keywords(&m.keywords);
    out.keywords = keywords.flat;
    out.hierarchical_keywords = keywords.hierarchical;
    out.capture_time = photo.captured.as_deref().and_then(DateTime::parse_iso);
    out.rating = (photo.rating > 0).then_some(photo.rating as i8);
    // Pixels are exported upright: orientation is baked in.
    out.orientation = Some(lightcraft_meta::Orientation::Normal);
    if !o.remove_location {
        out.gps = m.gps.map(|(latitude, longitude)| Gps { latitude, longitude, altitude: None });
    }
    if o.metadata == MetadataPolicy::All {
        out.model = text(&m.camera);
        out.lens_model = text(&m.lens);
        out.focal_length = m.focal_mm.map(f64::from);
        out.f_number = m.aperture.map(f64::from);
        out.exposure_time = lightcraft_catalog::parse_shutter_seconds(&m.shutter);
        out.iso = m.iso;
    }
    Some(out)
}

/// One exported file.
pub struct Exported {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Files written next to it, named like it with another extension: `(extension, bytes)` (the
    /// XMP sidecar of an `Original` export).
    pub sidecars: Vec<(&'static str, Vec<u8>)>,
    /// Set when the render was tried on the GPU and fell back to the CPU: why (see
    /// `docs/gpu-pipeline.md`). The file holds the CPU render either way.
    pub gpu_fallback: Option<String>,
}

/// Output size of photo `p` under `o` (its cropped full size when `o.resize` is `None`).
pub fn output_size(p: &lightcraft_catalog::Photo, o: &ExportOptions) -> (usize, usize) {
    let (w, h) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
    // whole pixels first: a crop's float noise (5999.999… for 6000) must not decide a half-pixel tie
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    match &o.resize {
        Some(r) => r.apply(w, h),
        None => (w as usize, h as usize),
    }
}

/// Render photo `id` at the requested size and encode it (or copy / convert its original for
/// [`ExportFormat::Original`] / [`ExportFormat::Dng`]).
pub fn export_photo(session: &mut crate::Session, id: lightcraft_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<Exported, String> {
    prepare_export(session, id, o, seq)?.run()
}

/// One photo's export, set up from the session ([`prepare_export`]). [`PreparedExport::run`] does
/// the heavy part (read, decode, render, encode) without the session, so it can run on another
/// thread.
pub struct PreparedExport {
    pub photo: lightcraft_catalog::PhotoId,
    pub file_name: String,
    /// The folder of the photo's original on disk ([`DestinationMode::SameAsOriginal`]); `None`
    /// for a demo photo or one in the browser's storage.
    pub source_dir: Option<String>,
    work: Work,
    /// The library's originals, which [`run_batch`] never writes over (shared by a batch).
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
    /// Estimated working memory of [`Self::run`] (bytes), held from [`crate::memory::export_gate`]
    /// while it runs beside other photos of a batch.
    weight: usize,
}

struct RenderWork {
    job: crate::media::RenderJob,
    /// The HDR rendition of a gain map JPEG (`job` renders its SDR base).
    hdr_job: Option<crate::media::RenderJob>,
    meta: Option<Metadata>,
    opts: ExportOptions,
}

enum Work {
    Render(Box<RenderWork>),
    /// [`ExportFormat::Original`] (the file's bytes + an XMP sidecar) or [`ExportFormat::Dng`] (the
    /// raw data re-encoded as a lossless DNG with the edits in its XMP).
    File {
        path: String,
        read: Option<crate::merge::ByteReader>,
        packet: String,
        dng: Option<DngCompression>,
        /// Lossy DNG: JPEG quality and the long edge to scale to (`None`: full size).
        quality: u8,
        lossy_edge: Option<u32>,
        /// The original must not carry a location ([`removes_location`]).
        remove_location: bool,
        label: String,
        size: (usize, usize),
    },
}

/// Set up the export of photo `id` at 1-based position `seq` of a batch. (For a whole batch use
/// [`prepare_batch`]: it looks at the library's originals once.)
pub fn prepare_export(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
) -> Result<PreparedExport, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    prepare_guarded(session, id, o, seq, guard)
}

/// [`prepare_export`] for each of `ids` in order (`{seq}` = position, from 1).
pub fn prepare_batch(session: &mut crate::Session, ids: &[lightcraft_catalog::PhotoId], o: &ExportOptions) -> Result<Vec<PreparedExport>, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    ids.iter().enumerate().map(|(i, id)| prepare_guarded(session, *id, o, i + 1, guard.clone())).collect()
}

fn prepare_guarded(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    let file_name = o.file_name_for(p, seq);
    let source_dir = original_folder(&p.source);
    // HDR output only for photos edited in HDR; the rest of the batch exports as usual
    let sdr_opts;
    let o = if o.hdr && !p.develop.hdr.enabled {
        sdr_opts = ExportOptions { hdr: false, ..o.clone() };
        &sdr_opts
    } else {
        o
    };
    let (full_px, long) = ((p.width as usize).saturating_mul(p.height as usize), p.width.max(p.height) as usize);
    let (work, weight) = if o.format.is_rendered() {
        let (w, h) = output_size(p, o);
        let (nw, nh) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
        let native_px = (nw.round().max(1.0) * nh.round().max(1.0)) as usize;
        let label = p.file_name.clone();
        let meta = export_metadata(p, &session.catalog, o);
        let gain_map = o.hdr_output() && o.format == ExportFormat::Jpeg;
        let job = session.export_job(id, w, h, o.effective_space(), o.effective_depth())?;
        let weight = render_weight(full_px, long, job.level, w.saturating_mul(h));
        if let Some(why) = too_big_for_memory((w, h), native_px, weight, crate::memory::total_ram()) {
            return Err(format!("{label}: {why}"));
        }
        let hdr_job = if gain_map { Some(session.export_job(id, w, h, o.effective_space(), OutputDepth::F32Hdr)?) } else { None };
        (Work::Render(Box::new(RenderWork { job, hdr_job, meta, opts: o.clone() })), weight)
    } else {
        let lightcraft_catalog::Source::File { path } = &p.source else {
            return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
        };
        let dng = (o.format == ExportFormat::Dng).then_some(o.dng_compression);
        // the file's bytes; a DNG also decodes the raw data (16-bit samples) and writes it again
        let weight = (p.file_size as usize).saturating_add(if dng.is_some() { full_px.saturating_mul(6) } else { 0 });
        let work = Work::File {
            path: path.clone(),
            read: session.media.file_bytes.clone(),
            packet: crate::sidecar::sidecar_packet_with(p, &session.catalog, removes_location(p, &session.catalog, o)),
            dng,
            quality: o.quality,
            lossy_edge: o.resize.is_some().then(|| {
                let (w, h) = output_size(p, o);
                u32::try_from(w.max(h)).unwrap_or(u32::MAX)
            }),
            remove_location: removes_location(p, &session.catalog, o),
            label: p.file_name.clone(),
            size: (p.width as usize, p.height as usize),
        };
        (work, weight)
    };
    Ok(PreparedExport { photo: id, file_name, source_dir, work, guard, weight })
}

/// The folder a photo's original is in, for [`DestinationMode::SameAsOriginal`]. None for a demo
/// photo, and on the web, where an original lives in the browser's storage, not in a folder.
fn original_folder(source: &lightcraft_catalog::Source) -> Option<String> {
    let lightcraft_catalog::Source::File { path } = source else { return None };
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let dir = std::path::Path::new(path).parent()?.to_string_lossy().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// Why an export of `out` pixels (`w`, `h`), from a photo of `native_px` pixels at its own size,
/// can't be made on a computer with `ram` bytes, given its working memory `weight`: an export
/// enlarged past the photo's size that needs more memory than the computer has would fail to
/// allocate, which ends the process rather than the export. Full-size and smaller exports, and
/// computers whose memory isn't known, are never refused.
fn too_big_for_memory(out: (usize, usize), native_px: usize, weight: usize, ram: Option<usize>) -> Option<String> {
    let ram = ram?;
    if out.0.saturating_mul(out.1) <= native_px || weight <= ram {
        return None;
    }
    let gb = |b: usize| b as f64 / (1u64 << 30) as f64;
    Some(format!(
        "exporting at {} × {} needs about {:.0} GB of memory, more than this computer has ({:.0} GB); choose a smaller size",
        out.0,
        out.1,
        gb(weight).ceil(),
        gb(ram)
    ))
}

/// Working memory of rendering a photo of `full_px` pixels (long edge `long`) from source level
/// `level` into `out_px` output pixels: the decoded source (linear RGB f32, 12 B/px) plus the
/// pipeline's planes and the encoded output (~36 B per output pixel). Measured: a 24 MP export
/// peaks at ~1.2 GB, one at 2048 px from the 2560 px preview at ~150 MB.
fn render_weight(full_px: usize, long: usize, level: crate::media::SourceLevel, out_px: usize) -> usize {
    let edge = level.max_edge().min(long.max(1));
    let scale = edge as f64 / long.max(1) as f64;
    let src_px = (full_px as f64 * scale * scale).min(usize::MAX as f64 / 64.0) as usize;
    src_px.saturating_mul(12).saturating_add(out_px.saturating_mul(36))
}

impl PreparedExport {
    pub fn run(self) -> Result<Exported, String> {
        let file_name = self.file_name;
        match self.work {
            Work::Render(w) => {
                let RenderWork { job, hdr_job, meta, opts } = *w;
                let result = job.run();
                let r = result.rendered?;
                let gpu_fallback = result.gpu_fallback;
                let bytes = match hdr_job {
                    Some(hj) => {
                        let hr = hj.run().rendered?;
                        let hdr = hr.deep.ok_or("the HDR render has no float samples")?;
                        encode_gain_map_jpeg(&r.image, &hdr, &opts, meta.as_ref())?
                    }
                    None => encode_rendered(&r, &opts, meta.as_ref())?,
                };
                Ok(Exported { file_name, bytes, width: r.image.width, height: r.image.height, sidecars: Vec::new(), gpu_fallback })
            }
            Work::File { path, read, packet, dng, quality, lossy_edge, label, size, remove_location } => {
                let bytes = match &read {
                    Some(r) => r(&path)?,
                    None => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                };
                let Some(dng) = dng else {
                    // the file is copied byte for byte: refuse rather than leak a location it embeds
                    if remove_location {
                        let metadata = lightcraft_meta::try_extract(&bytes)
                            .map_err(|e| format!("{label}: can't verify location metadata in this original file: {e}"))?;
                        if metadata.gps.is_some() {
                            return Err(format!(
                                "{label}: can't remove the location from an original file that embeds GPS; export as JPEG, TIFF or DNG instead"
                            ));
                        }
                    }
                    return Ok(Exported {
                        file_name,
                        bytes,
                        width: size.0,
                        height: size.1,
                        sidecars: vec![("xmp", packet.into_bytes())],
                        gpu_fallback: None,
                    });
                };
                if lightcraft_raw::probe(&bytes).is_none() {
                    return Err(format!("{label}: DNG export needs a raw photo"));
                }
                let mut raw = lightcraft_raw::decode(&bytes).map_err(|e| format!("{label}: {e}"))?;
                crate::camera_preview::with_starting_colour(&mut raw, &bytes);
                drop(bytes);
                let compression = match dng {
                    DngCompression::Lossless => lightcraft_raw::DngCompression::Lj92 { tile: 256 },
                    DngCompression::Deflate => lightcraft_raw::DngCompression::Deflate { tile: 256, half: false },
                    DngCompression::Uncompressed => lightcraft_raw::DngCompression::Uncompressed,
                    DngCompression::Lossy => {
                        lightcraft_raw::DngCompression::LossyJpeg { tile: 256, quality: quality.clamp(1, 100), long_edge: lossy_edge }
                    }
                };
                let dng = lightcraft_raw::write_dng(&raw, &lightcraft_raw::DngWriteOptions { xmp: Some(packet), compression, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                Ok(Exported { file_name, bytes: dng, width: raw.width, height: raw.height, sidecars: Vec::new(), gpu_fallback: None })
            }
        }
    }
}

/// How an exported DNG stores its raw data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DngCompression {
    /// Lossless JPEG (the usual DNG compression).
    #[default]
    Lossless,
    /// Deflate (zip) tiles with prediction.
    Deflate,
    Uncompressed,
    /// Lossy DNG: demosaiced linear data as JPEG tiles at the export quality, scaled to the resize
    /// setting when there is one (Lightroom's "Use Lossy Compression").
    Lossy,
}

/// The destination of a batch: a folder (with `ExportOptions::subfolder` and the conflict policy
/// applied to each file together with its sidecars; with [`DestinationMode::SameAsOriginal`] each
/// photo's own folder instead of `dir`), or one exact file path (single photo; an ordinary file
/// already there is replaced). Either way a catalogued original (or its sidecar)
/// is never written over: see [`crate::originals`].
#[derive(Clone, Debug, Default)]
pub struct Destination {
    pub dir: String,
    pub exact: Option<String>,
}

/// What happens after an export has written its files (Lightroom's Post-Processing): nothing, or
/// adding them to Apple Photos ([`ExportOptions::add_to_photos`]). Made by
/// [`crate::Session::after_export`] before the export starts, so an export asking for something
/// this session can't do fails before writing anything; for Photos it holds the import slot
/// (a [`crate::apple_photos::Reservation`]) until the files are handed over, and frees it when
/// dropped unused (the export failed or never ran). `Send`: a background export takes it to its
/// worker.
#[derive(Default)]
pub struct AfterExport {
    #[cfg(not(target_arch = "wasm32"))]
    apple_photos: Option<crate::apple_photos::Reservation>,
}

impl AfterExport {
    /// Add the files to Apple Photos with `runner`, into `album` when it isn't blank, reserving
    /// the import slot of `imports` now. (Front ends get this from
    /// [`crate::Session::after_export`], which checks the platform first.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn apple_photos(runner: crate::apple_photos::Runner, album: &str, imports: &crate::apple_photos::Imports) -> Result<Self, String> {
        Ok(Self { apple_photos: Some(imports.reserve(runner, album)?) })
    }

    /// Nothing to do.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_none(&self) -> bool {
        self.apple_photos.is_none()
    }

    /// Nothing to do.
    #[cfg(target_arch = "wasm32")]
    pub fn is_none(&self) -> bool {
        true
    }

    /// Do it now for an export's per-photo results ([`run_batch`]'s: only the files written
    /// count), waiting for Photos: for a worker thread (the app's background export) or a caller
    /// without a window (MCP, lightcraft-cli). `None` when there was nothing to do, else the
    /// outcome to report as the export's `applePhotos`: the finished job (`{job, running: false,
    /// requested, album}` with `{imported, ids, warning?}`, `{error}`, or `{skipped}` when the
    /// export was cancelled or wrote nothing). Never fails the export: its files are written
    /// either way.
    pub fn run(self, files: &[serde_json::Value], cancelled: bool) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.run(files, cancelled));
        }
        let _ = (files, cancelled);
        None
    }

    /// Like [`AfterExport::run`], without waiting: Photos works on a worker thread and the
    /// outcome is the running job (`{job, running: true, requested, album}`; its result comes
    /// from `export.photosImports`, and the desktop app announces it). For a caller on the UI
    /// thread, which must not wait minutes for Photos.
    pub fn start(self, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.start(files));
        }
        let _ = files;
        None
    }

    /// [`AfterExport::start`] where the session answers on a UI thread
    /// ([`crate::Session::apple_photos_background`]), else [`AfterExport::run`].
    pub fn finish(self, s: &crate::Session, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if s.apple_photos_background {
            return self.start(files);
        }
        let _ = s;
        self.run(files, false)
    }
}

impl crate::Session {
    /// What an export with `o` does after writing its files; an error, before anything is written,
    /// when it asks for Apple Photos and this platform or session has no way to reach it, or
    /// another import is reserved or running. For Photos the import slot is reserved from now on.
    pub fn after_export(&self, o: &ExportOptions) -> Result<AfterExport, String> {
        if !o.add_to_photos {
            return Ok(AfterExport::default());
        }
        if let Some(why) = apple_photos_unsupported() {
            return Err(why.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let runner = self.apple_photos.clone().ok_or_else(|| crate::apple_photos::unavailable().to_string())?;
            let mut after = AfterExport::apple_photos(runner, &o.photos_album, &self.apple_photos_imports)?;
            after.apple_photos = after.apple_photos.map(|r| r.shown_in(self.activity.clone()));
            Ok(after)
        }
        #[cfg(target_arch = "wasm32")]
        Err("Apple Photos is only available on macOS".into())
    }
}

/// Export `ids` in order ([`prepare_batch`] + [`run_batch`]), stopping at the first error.
pub fn export_batch(
    session: &mut crate::Session,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let items = prepare_batch(session, ids, o)?;
    run_batch(items, o, to, write, exists, true, &mut |_, _| true)
}

/// Write an exported or rendered file on disk: its folder is created if needed, and the file is
/// replaced atomically ([`lightcraft_catalog::safe_file::write_atomic_nosync`]: a temp file
/// renamed into place), so a failure part-way leaves any previous file intact and no truncated
/// one. Not synced to disk (issue #134): an export can always be made again from the original.
/// The native writer behind exports, renders and screenshots (app, CLI, MCP).
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, false)
}

/// [`write_file`], synced to disk: for a written file that becomes a library photo (Edit a Copy)
/// and so can't simply be exported again once the catalog points at it.
pub fn write_file_durable(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, true)
}

/// Write `path` only when nothing exists there: the destination is prepared as a complete temp
/// file and published with create-new semantics, so a file that appears after a caller's check is
/// never replaced. Screenshots of an initially absent path use this instead of the overwriting
/// writer.
pub fn write_file_new(path: &str, bytes: &[u8]) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    lightcraft_catalog::safe_file::write_new(p, bytes).map_err(|e| format!("{path}: {e}"))
}

fn write_with(path: &str, bytes: &[u8], durable: bool) -> Result<(), String> {
    use lightcraft_catalog::safe_file::{write_atomic, write_atomic_nosync};
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if durable { write_atomic(p, bytes) } else { write_atomic_nosync(p, bytes) }.map_err(|e| format!("{path}: {e}"))
}

/// The path of the sidecar with extension `ext` of the exported file `main`.
fn sidecar_path(main: &str, ext: &str) -> String {
    std::path::Path::new(main).with_extension(ext).to_string_lossy().to_string()
}

/// Run prepared exports in order: pick each one's path, and hand the bytes (and sidecars) to
/// `write`. `exists` tells whether a path is taken. The conflict policy applies to a file and its
/// sidecars as one: with Unique both get the same free name, with Skip the photo is skipped when
/// either is taken. A path that is a catalogued original (or its sidecar) is refused whatever the
/// policy. `progress(done, next file)` is called before each photo; returning false cancels the
/// rest. Returns one JSON object per photo: `{path, width, height, bytes, sidecars}`,
/// `{skipped: path}` or (unless `stop_on_error`) `{photo, file, error}`.
///
/// Several photos render side by side ([`export_parallelism`], within the memory of
/// [`crate::memory::export_gate`]), so one photo's decode, encode or GPU wait overlaps another's
/// render (issue #496). Paths, writes, results and `progress` still follow the batch order, and the
/// files are the same as one at a time.
pub fn run_batch(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let lanes = export_parallelism(items.len());
    run_batch_with(items, &mut Placer::new(o, to, write, exists, stop_on_error), progress, lanes)
}

/// How many photos of a batch of `n` render side by side. Each render already spreads its rows
/// over every core; the others fill the cores while it decodes, encodes or waits for the GPU.
/// One on wasm32 (no threads).
pub fn export_parallelism(n: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    (cores / 4).clamp(1, 4).min(n.max(1))
}

/// [`run_batch`] with `lanes` photos in flight (the calling thread is one of them).
fn run_batch_with(
    items: Vec<PreparedExport>,
    placer: &mut Placer<'_>,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    lanes: usize,
) -> Result<Vec<serde_json::Value>, String> {
    placer.single = items.len() == 1;
    if lanes <= 1 || items.len() <= 1 {
        for (i, item) in items.into_iter().enumerate() {
            if !progress(i, &item.file_name) {
                break;
            }
            let (photo, name, guard, source_dir) = (item.photo, item.file_name.clone(), item.guard.clone(), item.source_dir.clone());
            placer.place(photo, name, source_dir.as_deref(), &guard, run_gated(item))?;
        }
        return Ok(std::mem::take(&mut placer.out));
    }
    let heads: Vec<_> = items.iter().map(|i| (i.photo, i.file_name.clone(), i.guard.clone(), i.source_dir.clone())).collect();
    let queue = Queue {
        lane: std::sync::Mutex::new(Lane { items: items.into_iter().map(Some).collect(), next: 0, placed: 0, stop: false, done: Default::default() }),
        cv: std::sync::Condvar::new(),
        ahead: lanes,
    };
    std::thread::scope(|sc| {
        // stops the lanes however this thread leaves the scope: a panic in `progress` or the placer
        // would otherwise leave them waiting and the scope's join would never return
        let _stop = StopOnDrop(&queue);
        for k in 1..lanes {
            let q = &queue;
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("export-{k}")).spawn_scoped(sc, move || q.work()).is_err() {
                break;
            }
        }
        let placed = || {
            for (i, (photo, name, guard, source_dir)) in heads.into_iter().enumerate() {
                if !progress(i, &name) {
                    break;
                }
                let e = queue.result(i);
                placer.place(photo, name, source_dir.as_deref(), &guard, e)?;
            }
            Ok::<_, String>(())
        };
        placed()
    })?;
    Ok(std::mem::take(&mut placer.out))
}

/// Stops a [`Queue`] when dropped, also while unwinding from a panic.
struct StopOnDrop<'a>(&'a Queue);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Run one photo's export holding its working memory from [`crate::memory::export_gate`]; a panic
/// becomes that photo's error, not the batch's end.
fn run_gated(item: PreparedExport) -> Result<Exported, String> {
    let _held = crate::memory::export_gate().acquire(item.weight);
    let what = format!("exporting {}", item.file_name);
    crate::guard::catch(&what, || item.run()).and_then(|r| r)
}

/// The photos of a batch shared by its lanes: each lane takes the next one, at most
/// [`Queue::ahead`] past the last placed. That bounds the finished files waiting in memory for
/// their turn too (their working memory is given back to the gate as soon as they are encoded).
struct Queue {
    lane: std::sync::Mutex<Lane>,
    cv: std::sync::Condvar,
    ahead: usize,
}

struct Lane {
    items: Vec<Option<PreparedExport>>,
    /// The next photo to start.
    next: usize,
    /// Photos handed to the placer so far.
    placed: usize,
    stop: bool,
    done: std::collections::HashMap<usize, Result<Exported, String>>,
}

impl Lane {
    fn take_next(&mut self) -> Option<(usize, PreparedExport)> {
        let i = self.next;
        let item = self.items.get_mut(i)?.take()?;
        self.next += 1;
        Some((i, item))
    }
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait<'a>(&self, g: std::sync::MutexGuard<'a, Lane>) -> std::sync::MutexGuard<'a, Lane> {
        self.cv.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, i: usize, r: Result<Exported, String>) {
        self.lock().done.insert(i, r);
        self.cv.notify_all();
    }

    /// A lane thread: run photos until none is left or the batch stops.
    fn work(&self) {
        loop {
            let mut l = self.lock();
            let (i, item) = loop {
                if l.stop || l.next >= l.items.len() {
                    return;
                }
                if l.next < l.placed.saturating_add(self.ahead) {
                    match l.take_next() {
                        Some(t) => break t,
                        None => return,
                    }
                }
                l = self.wait(l);
            };
            drop(l);
            self.finish(i, run_gated(item));
        }
    }

    /// Photo `i`'s result, waiting for it; run here when no lane has started it yet.
    fn result(&self, i: usize) -> Result<Exported, String> {
        let mut l = self.lock();
        loop {
            if let Some(r) = l.done.remove(&i) {
                l.placed = i + 1;
                drop(l);
                self.cv.notify_all();
                return r;
            }
            if l.next == i {
                let Some((_, item)) = l.take_next() else {
                    return Err("export: photo missing from the batch".into());
                };
                drop(l);
                self.finish(i, run_gated(item));
                l = self.lock();
                continue;
            }
            l = self.wait(l);
        }
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }
}

/// Where a batch's files go: each photo's path (destination folder, subfolder, conflict policy,
/// sidecars, never an original) and its write, in batch order.
struct Placer<'a> {
    o: &'a ExportOptions,
    to: &'a Destination,
    single: bool,
    taken: std::collections::HashSet<String>,
    out: Vec<serde_json::Value>,
    write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &'a dyn Fn(&str) -> bool,
    stop_on_error: bool,
}

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{}/{b}", a.trim_end_matches('/')) }
}

impl<'a> Placer<'a> {
    fn new(
        o: &'a ExportOptions,
        to: &'a Destination,
        write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
        exists: &'a dyn Fn(&str) -> bool,
        stop_on_error: bool,
    ) -> Placer<'a> {
        Placer { o, to, single: false, taken: Default::default(), out: Vec::new(), write, exists, stop_on_error }
    }

    /// The folder photo `name` (its original in `source_dir`) is written to: the batch's folder or
    /// the photo's own ([`DestinationMode`]), plus the subfolder. An error for a photo with no
    /// folder of its own, or whose folder is gone: never a file written somewhere else instead.
    fn folder_for(&self, name: &str, source_dir: Option<&str>) -> Result<String, String> {
        let base = match self.o.destination_mode {
            DestinationMode::SpecificFolder => self.to.dir.clone(),
            DestinationMode::SameAsOriginal => {
                let Some(d) = source_dir else {
                    return Err(format!(
                        "{name}: the photo has no folder on disk (a demo photo or one in the browser's storage), so it can't be exported to the same folder as its original; choose a specific folder"
                    ));
                };
                if !(self.exists)(d) {
                    return Err(format!("{name}: the original's folder is missing ({d}); reconnect it or choose a specific folder"));
                }
                d.to_string()
            }
        };
        Ok(if self.o.subfolder.is_empty() { base } else { join_path(&base, &self.o.subfolder) })
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    fn place(
        &mut self,
        photo: lightcraft_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Exported, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // an exact file path needs no folder
        let dir = if self.to.exact.is_some() && self.single { Ok(String::new()) } else { self.folder_for(&name, source_dir) };
        let (o, exists) = (self.o, self.exists);
        let (dir, e) = match dir.and_then(|dir| e.map(|e| (dir, e))) {
            Ok(v) => v,
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => {
                self.out.push(json!({"photo": photo.0, "file": name, "error": err}));
                return Ok(());
            }
        };
        let dir = dir.as_str();
        // the exported file and its sidecars
        let group = |main: &str| std::iter::once(main.to_string()).chain(e.sidecars.iter().map(|(x, _)| sidecar_path(main, x))).collect::<Vec<_>>();
        let path = match self.to.exact.as_deref().filter(|_| self.single) {
            Some(p) => Ok(p.to_string()),
            None => {
                let path = join_path(dir, &e.file_name);
                let taken = &self.taken;
                let busy = |main: &str| group(main).iter().any(|p| taken.contains(p) || exists(p));
                if !busy(&path) {
                    Ok(path)
                } else {
                    match o.conflict {
                        Conflict::Overwrite => Ok(path),
                        Conflict::Skip => {
                            self.out.push(json!({"skipped": path}));
                            return Ok(());
                        }
                        Conflict::Unique => {
                            let (stem, ext) = e.file_name.rsplit_once('.').map_or((e.file_name.as_str(), None), |(a, b)| (a, Some(b)));
                            let name = |n: usize| join_path(dir, &ext.map_or(format!("{stem}-{n}"), |x| format!("{stem}-{n}.{x}")));
                            (2..1_000_000).map(name).find(|p| !busy(p)).ok_or_else(|| format!("{path}: no free file name"))
                        }
                    }
                }
            }
        };
        let file = path.clone().unwrap_or_else(|_| name.clone());
        let write = &mut *self.write;
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            write(&path, &e.bytes)?;
            let mut sidecars = Vec::new();
            for ((_, bytes), sc) in e.sidecars.iter().zip(files.iter().skip(1)) {
                write(sc, bytes)?;
                sidecars.push(sc.clone());
            }
            Ok((path, files, sidecars))
        });
        match written {
            Ok((path, files, sidecars)) => {
                self.taken.extend(files);
                let mut entry = json!({"path": path, "width": e.width, "height": e.height, "bytes": e.bytes.len(), "sidecars": sidecars});
                if let Some(why) = &e.gpu_fallback {
                    entry["gpuFallback"] = json!(why);
                }
                self.out.push(entry);
            }
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => self.out.push(json!({"photo": photo.0, "file": file, "error": err})),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
