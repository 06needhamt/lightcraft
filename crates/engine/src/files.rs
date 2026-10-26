//! Real files: probing (dimensions + metadata for import) and loading (decode → linear Rec.2020,
//! oriented, downscaled to the requested level). Standard formats go through `lightcraft-codecs`,
//! camera raws through `lightcraft-raw` (demosaic + DNG colour model).

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Meta};
use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

use crate::media::{DenoiseSpec, FileLoader, FileProbe, PairLoader, PreviewLoader, ProbeInfo, ReducedLoader};

fn meta_of(m: &lightcraft_meta::Metadata) -> (Meta, Option<String>) {
    let shutter = m.exposure_time.map(|t| if t >= 1.0 { format!("{t:.0}") } else { format!("1/{:.0}", 1.0 / t) }).unwrap_or_default();
    let camera = [m.make.clone().unwrap_or_default(), m.model.clone().unwrap_or_default()].join(" ").trim().to_string();
    let meta = Meta {
        camera,
        // (a camera that writes no lens name, like Nikon's DSLRs, still says what range the lens covers)
        lens: m.lens_model.clone().or_else(|| m.lens_spec.and_then(lightcraft_meta::lens_spec_name)).unwrap_or_default(),
        focal_mm: m.focal_length.map(|f| f as f32),
        aperture: m.f_number.map(|f| f as f32),
        shutter,
        iso: m.iso,
        location: m.sublocation.clone().unwrap_or_default(),
        city: m.city.clone().unwrap_or_default(),
        state: m.state.clone().unwrap_or_default(),
        country: m.country.clone().unwrap_or_default(),
        alt_text: m.alt_text.clone().unwrap_or_default(),
        extended_description: m.extended_description.clone().unwrap_or_default(),
        gps: m.gps.as_ref().map(|g| (g.latitude, g.longitude)),
        title: m.title.clone().unwrap_or_default(),
        caption: m.caption.clone().unwrap_or_default(),
        copyright: m.copyright.clone().unwrap_or_default(),
        copyright_status: lightcraft_catalog::CopyrightStatus::from_marked(m.copyright_marked),
        usage_terms: m.usage_terms.clone().unwrap_or_default(),
        copyright_url: m.copyright_url.clone().unwrap_or_default(),
        creator: m.artist.clone().unwrap_or_default(),
        // paths from `lr:hierarchicalSubject`, names from `dc:subject` that aren't their levels
        keywords: lightcraft_catalog::keywords::from_file(&m.keywords, &m.hierarchical_keywords),
        regions: m.regions.clone(),
        focus: m.focus.clone(),
    };
    (meta, m.capture_time.as_ref().map(|d| d.to_iso()))
}

/// Lens corrections embedded in a raw's `OpcodeList3` (`WarpRectilinear`, `WarpRectilinear2`, `FixVignetteRadial`: a DNG's own, or the
/// raw reader's equivalent of the camera's correction, e.g. Panasonic / Leica RW2 distortion), re-expressed for the
/// default-cropped, EXIF-oriented image. These are the only "profile" corrections LightCraft applies.
pub fn embedded_lens(raw: &lightcraft_raw::RawInfo) -> Option<lightcraft_develop::EmbeddedLens> {
    use lightcraft_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedWarp, EmbeddedWarpStage, WARP_STAGES};
    use lightcraft_geom::Point;
    use lightcraft_raw::Opcode;
    let (aw, ah) = (raw.active_area.width as f64, raw.active_area.height as f64);
    if aw < 2.0 || ah < 2.0 {
        return None;
    }
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let (cx0, cy0, cw, ch) =
        if c.width == 0 || c.height == 0 { (0.0, 0.0, aw, ah) } else { (c.x as f64, c.y as f64, c.width as f64, c.height as f64) };
    let long = cw.max(ch);
    // opcode centres are relative to the (uncropped) active area, in pixel-index units
    let centre = |rel: [f64; 2]| -> (Point, f64) {
        let (px, py) = (rel[0] * (aw - 1.0), rel[1] * (ah - 1.0));
        let m = [(0.0, 0.0), (aw - 1.0, 0.0), (0.0, ah - 1.0), (aw - 1.0, ah - 1.0)]
            .iter()
            .map(|&(x, y)| (x - px).hypot(y - py))
            .fold(0.0, f64::max)
            .max(1e-9);
        (Point::new((px + 0.5 - cx0) / cw, (py + 0.5 - cy0) / ch), m / long)
    };
    let mut lens = EmbeddedLens::default();
    // opcodes in list order, minus the ones the DNG 1.6 skip rule drops (a WarpRectilinear that follows an optional
    // WarpRectilinear2)
    let ops: Vec<&Opcode> = lightcraft_raw::opcodes::applied_ops(&raw.opcodes.list3).collect();
    // A DNG with a WarpRectilinear2 keeps every warp opcode as a chain of stages (each warps the one before's output);
    // without one the first WarpRectilinear is the lens warp, as it always was.
    let chain = ops.iter().any(|op| matches!(op, Opcode::WarpRectilinear2 { .. }));
    let mut stages = Vec::new();
    for op in ops {
        match op {
            Opcode::WarpRectilinear2 { planes, center, reciprocal, .. } if !planes.is_empty() => {
                let (center, radius) = centre(*center);
                let p = |i: usize| planes[i.min(planes.len() - 1)];
                stages.push(EmbeddedWarpStage { planes: [p(0), p(1), p(2)], center, radius, reciprocal: *reciprocal });
            }
            Opcode::WarpRectilinear { planes, center } if chain && !planes.is_empty() => {
                // WarpRectilinear is the even-power case: kr0 + kr1 r² + kr2 r⁴ + kr3 r⁶, no radius limit
                let (center, radius) = centre(*center);
                let p = |i: usize| {
                    let k = planes[i.min(planes.len() - 1)];
                    let mut s = [0.0; lightcraft_raw::opcodes::WR2_LEN];
                    s[0] = k[0];
                    s[2] = k[1];
                    s[4] = k[2];
                    s[6] = k[3];
                    s[15] = k[4];
                    s[16] = k[5];
                    s[18] = 1e6;
                    s
                };
                stages.push(EmbeddedWarpStage { planes: [p(0), p(1), p(2)], center, radius, reciprocal: false });
            }
            Opcode::WarpRectilinear { planes, center } if !chain && !planes.is_empty() && lens.warp.is_none() => {
                let (center, radius) = centre(*center);
                let p = |i: usize| planes[i.min(planes.len() - 1)];
                lens.warp = Some(EmbeddedWarp { planes: [p(0), p(1), p(2)], center, radius });
            }
            Opcode::FixVignetteRadial { k, center } if lens.vignette.is_none() => {
                let (center, radius) = centre(*center);
                lens.vignette = Some(EmbeddedVignette { k: *k, center, radius });
            }
            _ => {}
        }
    }
    // (more stages than a lens keeps: the first ones; real files carry two)
    for (slot, st) in lens.stages.iter_mut().zip(stages.into_iter().take(WARP_STAGES)) {
        *slot = Some(st);
    }
    if lens.warp.is_none() && lens.vignette.is_none() && !lens.has_stages() {
        return None;
    }
    Some(lightcraft_pipeline::optics::reorient_lens(&lens, raw.orientation, cw, ch))
}

fn ext_upper(name: &str) -> String {
    std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()
}

/// Probe a file's bytes: kind, dimensions (oriented), metadata. No pixel data is decompressed:
/// raws are described from their headers ([`lightcraft_raw::probe_info`]), other images too
/// ([`lightcraft_codecs::read_header`], which refuses truncated files but can't see damage inside
/// compressed data that is all there: such a file imports and shows as unreadable when rendered).
pub fn probe_bytes(name: &str, bytes: &[u8]) -> Result<ProbeInfo, String> {
    let content_hash = Some(lightcraft_preview::hash_bytes(bytes).to_string());
    let m = lightcraft_meta::extract(bytes);
    let (meta, captured) = meta_of(&m);
    if lightcraft_raw::probe(bytes).is_some() {
        let raw = match lightcraft_raw::probe_info(bytes).map_err(|e| preview_reason(bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // a raw variant we can't decode yet: describe it from its embedded preview
                let (w, h) = embedded_preview_size(bytes).ok_or(format!("unsupported raw ({why}) without an embedded preview"))?;
                return Ok(ProbeInfo {
                    width: w,
                    height: h,
                    format: ext_upper(name),
                    kind: MediaKind::Raw,
                    file_size: bytes.len() as u64,
                    captured,
                    meta,
                    as_shot_wb: None,
                    content_hash,
                    xmp: lightcraft_meta::embedded(bytes).xmp,
                    preview_only: Some(why),
                    ..Default::default()
                });
            }
            Err(Err(e)) => return Err(e),
        };
        let (w, h) = raw.developed_size();
        let (mut w, mut h) = (w.max(1) as u32, h.max(1) as u32);
        if raw.orientation.swaps_axes() {
            std::mem::swap(&mut w, &mut h);
        }
        let scale = WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
        let as_shot_wb = Some(scale.as_shot(lightcraft_raw::color::as_shot_white_xy_of(&raw)));
        let embedded_lens = crate::lens_profiles::for_raw(&raw, || m.clone());
        return Ok(ProbeInfo {
            embedded_lens,
            width: w,
            height: h,
            format: ext_upper(name),
            kind: MediaKind::Raw,
            file_size: bytes.len() as u64,
            captured,
            file_modified: None,
            meta,
            as_shot_wb,
            content_hash,
            xmp: lightcraft_meta::embedded(bytes).xmp,
            preview_only: None,
        });
    }
    let fmt = lightcraft_codecs::sniff(bytes).ok_or("unrecognized file format")?;
    if let Some(why) = fmt.not_decodable() {
        // e.g. "Heif files can't be opened: HEIC/HEIF support isn't included in this build of LightCraft"
        return Err(format!("{fmt:?} files can't be opened: {why}"));
    }
    // headers only (issue #367: decoding the pixels was nearly all of an import's CPU time)
    let header = lightcraft_codecs::read_header(bytes).map_err(|e| e.to_string())?;
    let o = Orientation::from_exif(header.orientation);
    let (mut w, mut h) = (header.width, header.height);
    if o.swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    let format = match ext_upper(name).as_str() {
        "JPG" | "JPEG" => "JPEG".to_string(),
        "" => format!("{fmt:?}").to_uppercase(),
        e => e.to_string(),
    };
    Ok(ProbeInfo {
        width: w,
        height: h,
        format,
        kind: MediaKind::Image,
        file_size: bytes.len() as u64,
        captured,
        file_modified: None,
        meta,
        as_shot_wb: None,
        content_hash,
        embedded_lens: None,
        xmp: None,
        preview_only: None,
    })
}

/// Why a raw that failed to decode should show its embedded preview instead (`Ok`), or the error
/// to report (`Err`). Variants we can't decode yet always fall back. So does any CR3 error: the CRX
/// decoder is verified on few bodies, and every CR3 opened from its embedded JPEG before it existed.
/// So does any other failure of a recognised raw (damaged or oversized raw data, unreadable TIFF
/// structure): its preview may still be intact. Only a file that is not a raw reports an error.
fn preview_reason(bytes: &[u8], e: lightcraft_raw::RawError) -> Result<String, String> {
    use lightcraft_raw::RawError;
    match e {
        RawError::Unsupported(why) => Ok(why),
        e if lightcraft_raw::probe(bytes) == Some(lightcraft_raw::RawFormat::Cr3) => Ok(format!("CR3 {e}")),
        RawError::NotRaw => Err(RawError::NotRaw.to_string()),
        e => Ok(e.to_string()),
    }
}

/// Sensor clip level (normalised) for highlight reconstruction.
const HIGHLIGHT_CLIP: f32 = 0.99;

/// How a raw's camera RGB becomes the linear Rec.2020 the pipeline takes.
pub(crate) struct CameraColourModel<'a> {
    /// White-balance multipliers (as shot).
    pub wb: [f32; 3],
    /// White-balanced camera RGB → linear Rec.2020: the camera matrix, or a look fitted to the camera's JPEG on top.
    pub matrix: lightcraft_color::Mat3,
    /// Baseline exposure, as a gain.
    pub gain: f32,
    /// A DNG profile's hue/saturation map and look table.
    pub tables: Option<&'a lightcraft_raw::profile::ProfileTables>,
    /// A fitted camera look's hue/saturation table.
    pub hue_sat: Option<&'a crate::camera_preview::HueSat<'a>>,
}

impl CameraColourModel<'_> {
    /// Rebuild sensor-clipped channels; fully clipped highlights become [`clipped_white`] of the matrix, so they
    /// come out of [`Self::apply`] neutral.
    pub fn rebuild_highlights(&self, img: &mut Rgb32f) {
        lightcraft_raw::highlight::reconstruct_with(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix));
    }

    /// [`Self::rebuild_highlights`] for a binned image whose clipped channels `mask` tells
    /// ([`lightcraft_raw::RawImage::develop_binned_masked`]).
    pub fn rebuild_highlights_masked(&self, img: &mut Rgb32f, mask: &[u8]) {
        lightcraft_raw::highlight::reconstruct_masked(img, self.wb, HIGHLIGHT_CLIP, clipped_white(&self.matrix), mask);
    }

    /// White balance, matrix, tables and baseline exposure, in place.
    pub fn apply(&self, img: &mut Rgb32f) {
        let (wb, m, gain) = (self.wb, self.matrix.to_f32(), self.gain);
        if let (None, Some(hue_sat)) = (self.tables, self.hue_sat) {
            // a fitted camera look: `pixel`, `LANES` pixels at a time so the table step vectorises (same bits)
            const LANES: usize = 16;
            lightcraft_raster::par_rows(&mut img.data, img.width, |_, row| {
                for chunk in row.chunks_mut(LANES) {
                    let mut rgb = [[0.0f32; LANES]; 3];
                    for (l, p) in chunk.iter().enumerate() {
                        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                        for (i, rgb) in rgb.iter_mut().enumerate() {
                            rgb[l] = (m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]) * gain;
                        }
                    }
                    let out = hue_sat.apply_lanes(rgb);
                    for (l, p) in chunk.iter_mut().enumerate() {
                        *p = [out[0][l], out[1][l], out[2][l]].map(|v| v.max(0.0));
                    }
                }
            });
            return;
        }
        img.map_in_place(|p| self.pixel(&m, p));
    }

    /// [`Self::apply`] to one pixel; `m` is the matrix as `f32`.
    fn pixel(&self, m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
        let (wb, gain) = (self.wb, self.gain);
        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        let rgb = [
            m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
            m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
            m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
        ];
        let rgb = match self.tables {
            Some(tables) => tables.apply(rgb, gain),
            None => rgb.map(|v| v * gain),
        };
        self.hue_sat.map_or(rgb, |h| h.apply(rgb)).map(|v| v.max(0.0))
    }
}

/// The white-balanced camera RGB that `model` (white-balanced camera RGB → linear Rec.2020) renders
/// neutral, as bright as it renders `[1, 1, 1]`: what fully clipped highlights are rebuilt to, so they come out
/// neutral whatever the colour model does to the camera's own neutral (a look fitted to the camera's JPEG or a
/// camera profile often tints it by a few per cent, which any darkening of the highlights would show).
/// `[1, 1, 1]` (the camera's own neutral, as before this existed) when the model can't be trusted with it: not
/// invertible or nearly so, an answer far from the camera's neutral (a sane colour model moves white by a few per
/// cent), or one that isn't neutral through the `f32` matrix the decode applies.
pub(crate) fn clipped_white(model: &lightcraft_color::Mat3) -> [f32; 3] {
    const CAMERA: [f32; 3] = [1.0; 3];
    /// Largest condition number (max row sum norm) of a model trusted for this.
    const CONDITION: f64 = 1e3;
    /// Largest factor between the answer and the camera's neutral, per channel.
    const SPREAD: f64 = 4.0;
    let norm = |m: &lightcraft_color::Mat3| m.0.iter().map(|row| row.iter().map(|v| v.abs()).sum::<f64>()).fold(0.0, f64::max);
    let Some(inverse) = model.inverse() else { return CAMERA };
    let condition = norm(model) * norm(&inverse);
    if !condition.is_finite() || condition > CONDITION {
        return CAMERA;
    }
    let luma = |v: [f64; 3]| {
        let c = model.apply(v);
        (0..3).map(|i| c[i] * f64::from(lightcraft_color::LUMA_2020[i])).sum::<f64>()
    };
    let n = inverse.apply([1.0; 3]);
    let (target, own) = (luma([1.0; 3]), luma(n));
    if !(target > 0.0 && own > 0.0 && target.is_finite() && own.is_finite()) {
        return CAMERA;
    }
    let white = n.map(|v| v * target / own);
    if !white.iter().all(|v| v.is_finite() && (1.0 / SPREAD..=SPREAD).contains(v)) {
        return CAMERA;
    }
    let white = white.map(|v| v as f32);
    // neutral through the matrix as the decode applies it
    let m = model.to_f32();
    let out: [f32; 3] = std::array::from_fn(|i| m[i][0] * white[0] + m[i][1] * white[1] + m[i][2] * white[2]);
    let (lo, hi) = (out[0].min(out[1]).min(out[2]), out[0].max(out[1]).max(out[2]));
    if lo > 0.0 && hi.is_finite() && hi - lo <= 1e-3 * hi { white } else { CAMERA }
}

/// The largest block size to bin a raw's mosaic by for a source of at most `max_edge` pixels:
/// the binned image must keep at least 90 % of `max_edge` (a 16 MP sensor still bins 2× for the
/// 2560 px preview). X-Trans can only bin 3× (its 6×6 pattern), and its full demosaic is ~5× the
/// cost of Bayer's, so 3× is accepted down to 75 % (a 24 MP X-Trans preview is then ~2000 px
/// instead of a ~1.2 s full demosaic; zooming in still uses the full-size source).
/// `None` = demosaic at full size.
pub fn bin_factor(raw: &lightcraft_raw::RawImage, max_edge: usize) -> Option<usize> {
    let c = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    let long = if c.width > 1 && c.height > 1 { c.width.max(c.height) } else { raw.active_area.width.max(raw.active_area.height) };
    let need = (max_edge.saturating_mul(9) / 10).max(1);
    let need3 = (max_edge.saturating_mul(3) / 4).max(1);
    [8usize, 6, 4, 3, 2].into_iter().find(|&k| raw.can_bin(k) && (long / k >= need || (k == 3 && !raw.can_bin(2) && long / k >= need3)))
}

/// Decode a file into a linear Rec.2020 image no larger than `max_edge`, oriented.
///
/// Runs on a rayon worker: its many short parallel loops then start on the worker's own queue
/// instead of each one waking the pool from outside and waiting for it (which costs more than the
/// loops themselves when the machine is busy).
pub fn load_bytes(bytes: &[u8], max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(|_| load_bytes_now(std::borrow::Cow::Borrowed(bytes), max_edge, None, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_bytes`] taking the file's bytes: a raw file's bytes are freed as soon as it is decoded
/// (less memory held while it is developed).
pub fn load_vec(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, false).map(|(img, _, info, _)| (img, info)))
}

/// [`load_vec`] for a thumbnail: a DNG that keeps reduced-resolution copies of its raw image (Lightroom's HDR and
/// panorama merges) develops the smallest that covers `max_edge` instead of the whole image. The flag says whether
/// it did: such a picture is not the full image, and an export never renders from it.
pub fn load_vec_reduced(bytes: Vec<u8>, max_edge: usize) -> Result<(Rgb32f, SourceInfo, bool), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, None, true).map(|(img, _, info, reduced)| (img, info, reduced)))
}

/// [`load_vec`] that also develops the photo's cached denoised picture (`spec`) from the same decode: the plain
/// picture, the denoised one (`None` when it cannot be read or does not fit this raw: the plain one is then all
/// there is) and the source facts. Both pictures are the same size.
pub fn load_vec_pair(bytes: Vec<u8>, max_edge: usize, spec: &DenoiseSpec) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo), String> {
    rayon::scope(move |_| load_bytes_now(std::borrow::Cow::Owned(bytes), max_edge, Some(spec), false).map(|(img, twin, info, _)| (img, twin, info)))
}

/// The denoised camera RGB of `raw` from its cached product, as the plain development would have it: the default
/// crop, and `factor` × `factor` blocks averaged into one pixel when the plain one was binned (plain means: the
/// plain development's clip mask tells highlight reconstruction which of them held clipped samples). `None` when
/// the product is missing, damaged, made from something else or not the size of this raw's active area.
fn denoised_camera_rgb(raw: &lightcraft_raw::RawImage, spec: &DenoiseSpec, factor: usize) -> Option<Rgb32f> {
    use lightcraft_denoise::product::{self, Window};
    let a = raw.active_area;
    let header = product::read_header_at(&spec.product).ok()?;
    if header.key != spec.key || (header.width, header.height) != (a.width, a.height) {
        return None;
    }
    if raw.opcodes.list3.is_empty() {
        let c = raw.develop_crop(a.width, a.height);
        let window = Window { x: c.x, y: c.y, width: c.width, height: c.height };
        product::read_window(&spec.product, &spec.key, Some(window), factor, None).ok()
    } else {
        // (the plain development is not binned either when the file has opcodes of this kind: `factor` is 1)
        let whole = product::read(&spec.product, &spec.key, 1).ok()?;
        Some(raw.finish_demosaiced(whole))
    }
}

fn load_bytes_now(
    bytes: std::borrow::Cow<'_, [u8]>,
    max_edge: usize,
    denoise: Option<&DenoiseSpec>,
    allow_reduced: bool,
) -> Result<(Rgb32f, Option<Rgb32f>, SourceInfo, bool), String> {
    if lightcraft_raw::probe(&bytes).is_some() {
        // a thumbnail (`allow_reduced`, never an export) of a DNG that keeps smaller copies of its raw image
        // (Lightroom's merges) develops the smallest that covers it; a denoised photo's twin needs the full mosaic
        let reduced = (allow_reduced && denoise.is_none() && max_edge < usize::MAX)
            .then(|| lightcraft_raw::decode_reduced(&bytes, max_edge).ok().flatten())
            .flatten();
        let used_reduced = reduced.is_some();
        let decoded = match reduced {
            Some(r) => Ok(r),
            None => lightcraft_raw::decode(&bytes),
        };
        let mut raw = match decoded.map_err(|e| preview_reason(&bytes, e)) {
            Ok(r) => r,
            Err(Ok(why)) => {
                // show the camera's embedded JPEG (rendered, not raw) until the variant is supported
                return load_embedded_preview(&bytes, max_edge)
                    .map(|(img, info)| (img, None, info, false))
                    .ok_or(format!("unsupported raw ({why}) without an embedded preview"));
            }
            Err(Err(e)) => return Err(e),
        };
        // Embedded lens corrections are applied by the pipeline ("Enable Profile Corrections"), not baked in
        // (removed before the camera look's binned sensor proxy too, which needs an empty `OpcodeList3`).
        let info = raw.info();
        let lens = crate::lens_profiles::for_raw(&info, || lightcraft_meta::extract(&bytes));
        // (the camera look still pairs the sensor with the camera JPEG through the warp the camera applied to it)
        let lens_geometry = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
        // A DNG's preview already has its lens opcodes applied. Keep them for its single tone fit,
        // then remove them again: the rendered source leaves those corrections to the pipeline.
        let dng_lens_ops = (raw.format == lightcraft_raw::RawFormat::Dng && raw.opcodes.list3.iter().any(|op| op.is_lens_correction()))
            .then(|| raw.opcodes.list3.clone());
        raw.opcodes.list3.retain(|op| !op.is_lens_correction());
        // Which scale white balance is edited on, and the as-shot white on it, from the file's own colour tags or
        // the camera's measured matrices (`Photo::relative_wb` decides the same from the catalog's camera)
        let scale = WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
        // the source's segmentation mattes (DNG semantic masks), read while the starting colour is fitted
        let ((xy, t, camera_look), mattes) = rayon::join(
            || {
                let removed = dng_lens_ops.map(|ops| std::mem::replace(&mut raw.opcodes.list3, ops));
                // With the opcodes restored, the proxy already includes vignetting; do not apply it twice.
                let embedded = if removed.is_some() { None } else { lens.as_ref() };
                let colour = crate::camera_preview::starting_colour(&mut raw, &bytes, lens_geometry.as_ref(), embedded);
                if let Some(ops) = removed {
                    raw.opcodes.list3 = ops;
                }
                colour
            },
            || dng_mattes(&bytes, &info),
        );
        drop(bytes);
        // Previews and thumbnails bin the mosaic straight to (about) the size they need; only
        // larger levels (exports, 1:1) demosaic the whole sensor.
        let t0 = web_time::Instant::now();
        let bin = bin_factor(&raw, max_edge);
        // (binned: block means, and which colours had a clipped sample in each block)
        let binned = match bin {
            Some(k) => raw.develop_binned_masked(k, HIGHLIGHT_CLIP).map_err(|e| e.to_string())?,
            None => None,
        };
        let factor = if binned.is_some() { bin.unwrap_or(1) } else { 1 };
        let (img, clip_mask) = match binned {
            Some((img, mask)) => (img, Some(mask)),
            None => {
                let method = if max_edge <= 600 { lightcraft_raw::Method::Bilinear } else { lightcraft_raw::Method::Ahd };
                (raw.develop(method).map_err(|e| e.to_string())?, None)
            }
        };
        // the same development from the denoised mosaic, when the photo has been denoised
        let twin = denoise.and_then(|spec| denoised_camera_rgb(&raw, spec, factor)).filter(|d| d.width == img.width && d.height == img.height);
        // the samples aren't needed any more (the colour model below reads only the tags)
        raw.data = lightcraft_raw::RawData::U16(Vec::new());
        let hue_sat = camera_look.as_ref().and_then(|p| p.hue_sat.as_ref()).and_then(crate::camera_preview::HueSat::new);
        // A DNG's own profile look (hue/saturation map, look table), DNG spec chapter 6.
        let tables = lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy));
        let colour = CameraColourModel {
            wb: t.wb,
            matrix: camera_look.as_ref().map(|p| p.matrix.mul(&t.matrix)).unwrap_or(t.matrix),
            gain: 2f32.powf(t.baseline_exposure as f32),
            tables: tables.as_ref(),
            hue_sat: hue_sat.as_ref(),
        };
        // camera RGB → what the pipeline takes: clipped highlights rebuilt, white balance and the colour model applied,
        // fitted to `max_edge` and upright (the same for the plain and the denoised picture)
        let finish = |mut img: Rgb32f| {
            let mut stages = vec![("develop", t0.elapsed())];
            stages.push(("transform", t0.elapsed()));
            match &clip_mask {
                Some(mask) => colour.rebuild_highlights_masked(&mut img, mask),
                None => colour.rebuild_highlights(&mut img),
            }
            stages.push(("highlights", t0.elapsed()));
            colour.apply(&mut img);
            stages.push(("colour", t0.elapsed()));
            // a full-size render keeps the picture (fitting it to its own size would copy it, ~390 MB at 33 MP)
            let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Box) } else { img };
            stages.push(("fit", t0.elapsed()));
            let img = img.into_oriented(raw.orientation);
            stages.push(("orient", t0.elapsed()));
            (img, stages)
        };
        let (img, stages) = finish(img);
        if lightcraft_pipeline::profiling() {
            let mut prev = std::time::Duration::ZERO;
            let parts: Vec<String> = stages
                .iter()
                .map(|(n, t)| {
                    let d = *t - prev;
                    prev = *t;
                    format!("{n} {:.1}", d.as_secs_f64() * 1e3)
                })
                .collect();
            eprintln!("[profile] raw source {}×{} (max {max_edge}, ms after decode): {}", img.width, img.height, parts.join(", "));
        }
        let twin = twin.map(|picture| finish(picture).0);
        // White balance re-evaluates the file's own colour model (when it has one and no file-local look matrix sits
        // on top of it). A model's measured matrices never do: they only give the Kelvin scale its numbers, and the
        // picture is adapted from the as-shot white as every fitted look is (so settings from before that scale
        // existed render exactly as they did, `lightcraft_pipeline::local::legacy_white`).
        let camera_color = (scale.own_matrix && !t.matrix_is_fallback && (camera_look.is_none() || raw.format == lightcraft_raw::RawFormat::Dng))
            .then(|| {
                let tags = lightcraft_raw::ColorData { profile: Default::default(), ..raw.color.clone() };
                Arc::new(lightcraft_pipeline::CameraColor { tags, developed_for: xy })
            });
        let camera_tone = camera_look.as_ref().map(|p| p.tone).or_else(|| raw.color.profile.tone_curve.as_ref().and_then(dng_tone_curve));
        let local_tone = local_tone(&raw);
        let (temp, tint) = scale.as_shot(xy);
        return Ok((
            img,
            twin,
            SourceInfo {
                raw: true,
                as_shot_temp: temp,
                as_shot_tint: tint,
                lens,
                relative_wb: scale.relative,
                legacy_relative_wb: scale.legacy_relative,
                camera_color,
                camera_tone,
                mattes,
                local_tone,
                // (the catalog knows the original's size: `media::source_info`)
                native_long: None,
            },
            used_reduced,
        ));
    }
    let d = lightcraft_codecs::decode(&bytes, fit_box(max_edge)).map_err(|e| e.to_string())?;
    drop(bytes);
    let img = d.to_working();
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Ok((img.into_oriented(Orientation::from_exif(d.orientation)), None, SourceInfo::default(), false))
}

/// Which white-balance scale a raw is edited on, decided from the file alone, the way the catalog decides it from
/// the photo's format and camera (`lightcraft_catalog::relative_wb_camera`; a test keeps the two in step).
///
/// A file with colour matrices of its own (DNG) has an absolute scale from its tags. A file without
/// (`camera_preview::file_local_look`) has one when its camera's matrices are measured
/// (`lightcraft_raw::spectral`), else when its camera's white-balance presets give one
/// (`lightcraft_raw::kelvin_presets`): either turns its as-shot multipliers into Kelvin and tint, nothing else — the
/// look stays the file's fit. Any other raw is edited relative to its as-shot look (6500 K / 0 means as shot).
pub(crate) struct WbScaleOf {
    own_matrix: bool,
    relative: bool,
    /// Kelvin through the camera's measured matrices or presets: edited on the relative scale before those were used.
    legacy_relative: bool,
    /// The as-shot white through the measured matrices or the presets, when that is where the scale comes from.
    scale_white: Option<lightcraft_color::Xy>,
    /// The camera's measured matrices, when they give the scale.
    camera: Option<&'static lightcraft_raw::spectral::Camera>,
}

/// The white balance a DNG of a raw's camera RGB (Photo Merge) declares so that it shows as its source does
/// ([`WbScaleOf::dng_white`]).
pub(crate) enum DngWhite {
    /// Colour tags with the camera's measured matrices: the DNG reads every white through them, as the source does.
    Matrices(Box<lightcraft_raw::ColorData>),
    /// The white the source's scale shows at As Shot.
    At(lightcraft_color::Xy),
}

impl WbScaleOf {
    pub(crate) fn new(
        format: lightcraft_raw::RawFormat,
        color: &lightcraft_raw::ColorData,
        wb_multipliers: Option<[f32; 3]>,
        meta: &lightcraft_meta::Metadata,
    ) -> Self {
        let own_matrix = lightcraft_raw::color::has_matrix(color);
        let fitted = crate::camera_preview::file_local_look(format) && !own_matrix;
        let model = meta.model.as_deref().filter(|_| fitted);
        let camera = model.and_then(|model| lightcraft_raw::spectral::find(meta.make.as_deref(), model));
        let presets = camera.is_none() && model.is_some_and(|model| lightcraft_raw::kelvin_presets::covers(meta.make.as_deref(), model));
        let scale_white = match camera {
            Some(c) => lightcraft_raw::spectral::as_shot_white_through(c, color, wb_multipliers),
            None if presets => lightcraft_raw::kelvin_presets::as_shot_white(color, wb_multipliers),
            None => None,
        };
        let covered = camera.is_some() || presets;
        WbScaleOf { own_matrix, relative: fitted && !covered, legacy_relative: fitted && covered, scale_white, camera }
    }

    /// What a DNG of this raw's camera RGB declares so that it shows the white balance as this scale does: the
    /// camera's measured matrices when they give the scale, else the white the scale shows at As Shot (the presets'
    /// reading; 6500 K / 0 on the relative scale; else `own`, the white the file's tags give).
    pub(crate) fn dng_white(&self, color: &lightcraft_raw::ColorData, own: lightcraft_color::Xy) -> DngWhite {
        if let Some(camera) = self.camera {
            let mut c = lightcraft_raw::ColorData { profile: Default::default(), kelvin_presets: None, ..color.clone() };
            if camera.fill(&mut c) {
                return DngWhite::Matrices(Box::new(c));
            }
        }
        if self.relative {
            return DngWhite::At(lightcraft_color::cct::temp_tint_to_xy(6500.0, 0.0));
        }
        DngWhite::At(self.scale_white.unwrap_or(own))
    }
    /// The as-shot temperature and tint on the file's scale: the relative scale's reference, the white through
    /// the measured matrices or the presets, else `own` (the white the file's own tags give). Whole numbers, as the
    /// catalog keeps them. A covered camera whose file names no white (no multipliers, or no presets) falls back to
    /// `own`, D65.
    fn as_shot(&self, own: lightcraft_color::Xy) -> (f64, f64) {
        if self.relative {
            return (6500.0, 0.0);
        }
        let (t, tint) = xy_to_temp_tint(self.scale_white.unwrap_or(own));
        (t.round(), tint.round())
    }
}

/// The raw's gain table map with where its developed picture (the default crop, oriented) sits in
/// the active area: rendered when the photo's "Camera local tone mapping" option is on.
fn local_tone(raw: &lightcraft_raw::RawImage) -> Option<Arc<lightcraft_pipeline::local_tone::LocalTone>> {
    let map = raw.color.profile.gain_table_map.clone()?;
    let a = raw.active_area;
    let (aw, ah) = (a.width.max(1) as f64, a.height.max(1) as f64);
    let c = raw.develop_crop(a.width, a.height);
    let rect = [c.x as f64 / aw, c.y as f64 / ah, c.width as f64 / aw, c.height as f64 / ah];
    let placement = lightcraft_raw::gaintable::SourcePlacement { rect, orientation: raw.orientation };
    Some(Arc::new(lightcraft_pipeline::local_tone::LocalTone { map, placement }))
}

/// The semantic masks of a DNG that AI masks understand, over the developed image (default crop,
/// oriented like it). Person mattes without a confidently selected pixel are left out (the
/// Subject mask then falls back to its heuristic); a sky matte counts even when empty (no sky).
fn dng_mattes(bytes: &[u8], raw: &lightcraft_raw::RawInfo) -> Option<Arc<lightcraft_pipeline::masks::Mattes>> {
    use lightcraft_pipeline::masks::{MatteKind, Mattes};
    if raw.format != lightcraft_raw::RawFormat::Dng {
        return None;
    }
    let mut mattes = Mattes::default();
    for m in lightcraft_raw::semantic_masks(bytes) {
        let Some(kind) = matte_kind(&m.name) else { continue };
        let Some(img) = m.developed(raw.active_area, raw.crop) else { continue };
        if kind != MatteKind::Sky && !img.data.iter().any(|&v| v >= 128) {
            continue;
        }
        mattes.push(kind, img.into_oriented(raw.orientation));
    }
    (!mattes.is_empty()).then(|| Arc::new(mattes))
}

/// What a semantic mask selects, by its `SemanticName`: Apple's (iPhone ProRAW) are named after
/// their auxiliary image types, `urn:com:apple:photo:<year>:aux:<type>`. Other names: `None`.
fn matte_kind(name: &str) -> Option<lightcraft_pipeline::masks::MatteKind> {
    use lightcraft_pipeline::masks::MatteKind::*;
    let kind = name.strip_prefix("urn:com:apple:photo:")?.rsplit(':').next()?;
    Some(match kind {
        "semanticskymatte" => Sky,
        "semanticskinmatte" => Skin,
        "semantichairmatte" => Hair,
        "semanticteethmatte" => Teeth,
        "semanticglassesmatte" => Glasses,
        "portraiteffectsmatte" => Person,
        _ => return None,
    })
}

/// A DNG `ProfileToneCurve` (linear in, linear out, 1.0 = white after exposure compensation) as the
/// finish stage's camera tone curve: 32 knots, log-spaced over 12 stops below white; above white
/// the camera tone's shoulder continues it.
pub(crate) fn dng_tone_curve(curve: &lightcraft_raw::profile::ToneCurve) -> Option<lightcraft_pipeline::tone::CameraTone> {
    let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
        [x, curve.eval(x).min(0.9995)]
    });
    lightcraft_pipeline::tone::CameraTone::new(knots)
}

/// Orientation for an embedded preview: its own EXIF orientation when it has one, else the raw file's.
fn preview_orientation(raw_bytes: &[u8], jpeg_orientation: u16) -> Orientation {
    if jpeg_orientation > 1 {
        return Orientation::from_exif(jpeg_orientation);
    }
    lightcraft_meta::extract(raw_bytes).orientation.unwrap_or(Orientation::Normal)
}

/// What stands in for a raw file we can't decode: its largest embedded JPEG (with `covering`, the smallest one that is
/// at least that big, [`lightcraft_raw::embedded_preview_covering`]), else, for a TIFF-based raw, the file's own first
/// image when the TIFF reader can show it (a reduced RGB copy: the only preview some containers carry). Returns image
/// bytes for [`lightcraft_codecs::decode`].
fn stand_in_image(bytes: &[u8], covering: Option<usize>) -> Option<std::borrow::Cow<'_, [u8]>> {
    let jpeg = match covering {
        Some(edge) => lightcraft_raw::embedded_preview_covering(bytes, u32::try_from(edge).unwrap_or(u32::MAX)),
        None => lightcraft_raw::embedded_preview(bytes),
    };
    if let Some(jpeg) = jpeg {
        return Some(std::borrow::Cow::Owned(jpeg));
    }
    let undecodable = lightcraft_raw::probe(bytes).is_some_and(|f| !f.is_supported());
    (undecodable && lightcraft_codecs::sniff(bytes).is_some_and(|f| f == lightcraft_codecs::Format::Tiff))
        .then_some(std::borrow::Cow::Borrowed(bytes))
}

/// Shared colour interpretation for quick previews, unsupported RAW fallback and camera-look fitting (`covering`: see
/// [`stand_in_image`]).
pub(crate) fn decode_raw_preview(
    bytes: &[u8],
    opts: lightcraft_codecs::DecodeOptions,
    covering: Option<usize>,
) -> Option<lightcraft_codecs::Decoded> {
    let jpeg = stand_in_image(bytes, covering)?;
    // the colour space is a property of the JPEG; a TIFF stand-in (the file itself, or a Hasselblad RGB preview
    // wrapped as a TIFF) and a JPEG XL preview are read by their own decoders
    let is_jpeg = jpeg.starts_with(&[0xff, 0xd8]);
    match lightcraft_raw::embedded_preview_color_space(bytes).filter(|_| is_jpeg) {
        Some(space) => {
            let space = match space {
                lightcraft_raw::PreviewColorSpace::Srgb => lightcraft_codecs::NamedSpace::Srgb,
                lightcraft_raw::PreviewColorSpace::AdobeRgb => lightcraft_codecs::NamedSpace::AdobeRgb,
            };
            lightcraft_codecs::decode_jpeg_with_fallback(&jpeg, opts, space).ok()
        }
        None => lightcraft_codecs::decode(&jpeg, opts).ok(),
    }
}

/// Oriented size of the embedded preview of a raw file.
fn embedded_preview_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let jpeg = stand_in_image(bytes, None)?;
    // headers only, as for other images (issue #367): decoding a full-size preview just for its
    // size took about a second per file in an import; a decode remains the fallback
    let (mut w, mut h, orientation) = match lightcraft_codecs::read_header(&jpeg) {
        Ok(header) => (header.width, header.height, header.orientation),
        Err(_) => {
            let d = lightcraft_codecs::decode(&jpeg, lightcraft_codecs::DecodeOptions::fit(64, 64)).ok()?;
            (d.source_width, d.source_height, d.orientation)
        }
    };
    if preview_orientation(bytes, orientation).swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    Some((w, h))
}

/// The embedded preview of a raw file as a working-space image no larger than `max_edge`, oriented.
pub fn load_embedded_preview(bytes: &[u8], max_edge: usize) -> Option<(Rgb32f, SourceInfo)> {
    // always the largest JPEG: a source's geometry must not depend on its size level (previews match exports)
    let d = decode_raw_preview(bytes, fit_box(max_edge), None)?;
    let img = d.to_working();
    let img = if img.width.max(img.height) > max_edge { fit(&img, max_edge, max_edge, Filter::Mitchell) } else { img };
    Some((img.oriented(preview_orientation(bytes, d.orientation)), SourceInfo::default()))
}

/// The embedded preview of a raw file for display (sRGB, oriented, no larger than `max_edge`): the
/// loupe and grid show it until the raw itself has been developed ([`crate::media::QuickJob`]). The smallest
/// embedded JPEG that covers `max_edge` is decoded, not the largest.
pub fn embedded_preview_srgb(bytes: &[u8], max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    let mut d = decode_raw_preview(bytes, fit_box(max_edge), Some(max_edge))?;
    if d.image.width.max(d.image.height) > max_edge {
        d.image = fit(&d.image, max_edge, max_edge, Filter::Box);
        d.alpha = None;
    }
    let o = preview_orientation(bytes, d.orientation);
    Some(d.to_srgb8().oriented(o))
}

/// Decode options fitting into a `max_edge` square; `usize::MAX` (a full-size load) or any edge past
/// `u32::MAX` asks for the largest box rather than wrapping to a small one.
fn fit_box(max_edge: usize) -> lightcraft_codecs::DecodeOptions {
    let e = u32::try_from(max_edge).unwrap_or(u32::MAX);
    lightcraft_codecs::DecodeOptions::fit(e, e)
}

/// Filesystem-backed embedded-preview hook (native).
///
/// The quick path reads the file's header and its embedded preview and nothing else (see
/// [`preview_window`]); a container the header cannot point into is planned from its tags
/// (`lightcraft_raw::sparse::preview_plan`, the same module the import path probes with), and only a
/// file the planner cannot describe falls back to the whole-file path. The memory gate still counts
/// the whole file, which is what the decode of one can hold.
pub fn fs_preview_loader() -> PreviewLoader {
    Arc::new(|path: &str, max_edge: usize| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(len.saturating_mul(2)) } else { gate.acquire_urgent(len.saturating_mul(2)) };
        if let Some(image) = preview_window(path, max_edge)
            .ok()
            .flatten()
            .and_then(|(header, window, read)| quick_image_srgb(&header, &window, read.adobe_rgb, max_edge))
        {
            return Some(image);
        }
        embedded_preview_srgb(&std::fs::read(path).ok()?, max_edge)
    })
}

/// What [`preview_window`] read from a file, so a caller can see what a preview cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewRead {
    /// Bytes read.
    pub read: u64,
    /// The file's size.
    pub total: u64,
    /// Whether the preview came from a bounded read (`false`: the whole file was read, which is what
    /// every preview cost before the header path existed).
    pub header_path: bool,
    /// Whether the sparse planner named the ranges, rather than a format's own header fields: the
    /// path a Canon CR3 and the TIFF raws whose preview tag sits past the prefix take (`false` on
    /// the header path, and on the whole-file fallback).
    pub planned: bool,
    /// Adobe RGB when the container says the preview is in it and the JPEG doesn't say otherwise
    /// (`decode_raw_preview` applies the same declaration): the quick decode applies it too.
    pub adobe_rgb: bool,
}

impl PreviewRead {
    /// The share of the file that was read, `0.0`..=`1.0`.
    pub fn fraction(self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.read as f64 / self.total as f64).clamp(0.0, 1.0)
    }
}

/// The file's header and its embedded preview, read without the rest of the file: `(header bytes,
/// preview bytes, what was read)`. `None` when the header points at no preview this build can find,
/// which is the caller's cue to read the file whole. `max_edge` is the size the preview is for: where a file keeps
/// several JPEGs (the plan route), the window is the one [`embedded_preview_srgb`] decodes for that size, the smallest
/// that covers it ([`lightcraft_raw::embedded_preview_covering`]).
///
/// Why it exists: a 56 MB Fuji RAF keeps its JPEG in the first 5.6 MB (measured: `DSCF9492.RAF`),
/// so reading the file whole to extract it costs ten times the bytes — and on a network share ten
/// times the wait (measured on that file: 2.5 s local against 25.6 s over SMB). `embedded_preview`
/// still needs the whole container, so the header comes back with the window; for a preview taken
/// out of a container that carries no preview colour space of its own (Nikon's maker note) the
/// whole-file fallback stays in charge.
pub fn preview_window(path: &str, max_edge: usize) -> Result<Option<(Vec<u8>, Vec<u8>, PreviewRead)>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let total = file.metadata().map_err(|e| format!("{path}: {e}"))?.len();
    let mut prefix = read_prefix(&mut file, path, total)?;

    // What the header itself declares, before the container reader is asked: it clamps a slice to
    // the buffer it was given, so a prefix that stops inside the declared preview yields a
    // self-consistently short range that looks trustworthy (measured on a real 56 MB DSCF9492.RAF:
    // a 4 KiB prefix reports 3948 bytes where the file holds 5.6 MB). Reading the declared extent
    // first costs nothing — it lives in the first bytes of every format covered here.
    if let Some(extent) = raf_preview_extent(&prefix)
        && extent > prefix.len() as u64
    {
        prefix = read_prefix_at(&mut file, path, extent.min(total))?;
    }
    let Some(range) = lightcraft_raw::bounded_preview(&prefix) else {
        // The header carries no pointer this path reads — a Canon CR3's boxes, a TIFF raw whose
        // preview tag sits past the prefix, the containers whose preview only a scan finds: ask the
        // sparse planner which ranges `embedded_preview` reads before handing the caller the
        // whole-file fallback.
        return planned_preview(&mut file, path, total, max_edge);
    };
    // A preview at the file's end may have been padded past it: clamp rather than fail.
    let end = range.offset.saturating_add(range.window_len).min(total);
    let len = usize::try_from(end.saturating_sub(range.offset)).map_err(|_| format!("{path}: preview too large"))?;
    // When the prefix already covers the range (the usual case: the prefix was grown to the declared
    // extent), take the window out of it. Reading those bytes twice would double a 5.6 MB preview.
    let prefix_len = prefix.len() as u64;
    let from_prefix = usize::try_from(range.offset).ok().and_then(|start| prefix.get(start..start.checked_add(len)?)).map(<[u8]>::to_vec);
    let (window, bytes_read) = match from_prefix {
        Some(window) => (window, prefix_len),
        None => {
            let mut window = vec![0u8; len];
            file.seek(SeekFrom::Start(range.offset)).map_err(|e| format!("{path}: {e}"))?;
            file.read_exact(&mut window).map_err(|e| format!("{path}: {e}"))?;
            (window, prefix_len + len as u64)
        }
    };

    // A lying pointer must not cost the user their preview: hand the caller the whole-file fallback.
    if lightcraft_raw::embedded_preview_from_range(&window, range).is_none() {
        return Ok(None);
    }
    let read = PreviewRead { read: bytes_read, total, header_path: true, planned: false, adobe_rgb: false };
    // On a network share the bytes are the wait, so say what this preview actually cost. A window
    // taken from the prefix cost nothing beyond the prefix itself.
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] preview {path}: read {} of {} bytes (header {}, window {})", read.read, total, prefix_len, window.len());
    }
    Ok(Some((prefix, window, read)))
}

/// The most times the sparse planner may name ranges before the preview gives up: the plan is
/// monotone (it never asks for a range twice), so a real file needs a handful.
const MAX_PREVIEW_PLAN_ROUNDS: usize = 8;
/// Tenths of a file the planner may have read before the preview gives up and the whole-file path
/// takes over. The import path bounds its own loop the same way; a "preview" that wants most of the
/// file has stopped being one.
const MAX_PREVIEW_PLAN_SHARE: u64 = 6;
/// The largest file the planner path builds its buffer for. The raws it serves are far below this;
/// a file past it is read whole as before rather than allocating its length up front.
const MAX_PREVIEW_PLAN_BYTES: u64 = 1 << 30;
/// The header the quick path is handed on the planner route. The header route grows its prefix to
/// the preview's declared extent, so `quick_image_srgb` can read the file's own EXIF orientation out
/// of it when the preview JPEG carries none; this route reads a bounded prefix instead, big enough
/// for IFD0 (which is where that tag lives) and for a CR3's metadata boxes.
const PLAN_PREFIX: u64 = 64 * 1024;

/// The preview of a container the header-only path cannot answer for, from the byte ranges
/// [`lightcraft_raw::sparse::preview_plan`] says [`lightcraft_raw::embedded_preview`] reads.
///
/// The planner is asked about a buffer of the file's whole length in which only the ranges named so
/// far hold bytes, and only those ranges are read: the sensor data in the bulk ranges is not. The
/// buffer is the file's length, which the loader's memory gate already counts, and its untouched
/// pages stay zero and are never written — though on macOS a freed buffer of that size may stay in
/// the allocator's cache rather than going back to the OS (measured on a 38k-photo import, #832), so
/// this path's cost is the bytes read, not a promise about RSS.
///
/// `None` — the caller's cue to read the file whole, exactly as when the header declines — when the
/// planner says it cannot describe the file (NEF, ORF and PEF probe from samples; a structure it
/// does not trust), when the ranges it names do not fit the buffer or want most of the file, or when
/// the quick path could not reproduce what the whole-file path shows (the two container facts
/// below). The planner's contract is the one its corpus test asserts over real files (what it loaded
/// gives what the whole file gives), so a window that still fails to decode ends in the same
/// whole-file fallback in [`fs_preview_loader`], never in a wrong picture.
fn planned_preview(file: &mut std::fs::File, path: &str, total: u64, max_edge: usize) -> Result<Option<(Vec<u8>, Vec<u8>, PreviewRead)>, String> {
    use std::io::{Read, Seek, SeekFrom};

    if total > MAX_PREVIEW_PLAN_BYTES {
        return Ok(None);
    }
    let Ok(len) = usize::try_from(total) else { return Ok(None) };
    let mut buf = vec![0u8; len];
    let header = read_prefix_at(file, path, PLAN_PREFIX.min(total))?;
    let Some(head) = buf.get_mut(..header.len()) else { return Ok(None) };
    head.copy_from_slice(&header);
    let mut loaded = lightcraft_raw::sparse::Ranges::new();
    loaded.insert(0, header.len() as u64);
    for _ in 0..MAX_PREVIEW_PLAN_ROUNDS {
        match lightcraft_raw::sparse::preview_plan(&buf, &loaded) {
            lightcraft_raw::sparse::HeaderPlan::Need(ranges) => {
                let wanted = loaded.total().saturating_add(ranges.total());
                if wanted.saturating_mul(10) > total.saturating_mul(MAX_PREVIEW_PLAN_SHARE) {
                    return Ok(None);
                }
                for &(start, end) in ranges.spans() {
                    let (Ok(a), Ok(b)) = (usize::try_from(start), usize::try_from(end)) else { return Ok(None) };
                    let Some(dst) = buf.get_mut(a..b) else { return Ok(None) };
                    file.seek(SeekFrom::Start(start)).map_err(|e| format!("{path}: {e}"))?;
                    file.read_exact(dst).map_err(|e| format!("{path}: {e}"))?;
                    loaded.insert(start, end);
                }
            }
            lightcraft_raw::sparse::HeaderPlan::Ready => {
                // the plan loads every preview's strips, so the JPEG picked for `max_edge` is the whole file's pick
                let edge = u32::try_from(max_edge).unwrap_or(u32::MAX);
                let Some(window) = lightcraft_raw::embedded_preview_covering(&buf, edge) else { return Ok(None) };
                // The quick path hands the JPEG to the decoder alone, so what the *container* says
                // about the preview has to be something the bytes it is handed can show. Two container
                // facts change the pixels and are not in the JPEG: the preview's colour space (which
                // the whole-file path applies through `decode_raw_preview`) and, when the JPEG carries
                // no orientation of its own, the file's EXIF orientation. The colour space is read
                // here and handed on ([`PreviewRead::adobe_rgb`]: a Sony α1 writes Adobe RGB previews,
                // which used to send each of them to the whole-file path, issue #1440); a container
                // that says sRGB agrees with the JPEG-only decode (its own declaration, else sRGB).
                // Where the orientation is present and the header cannot answer for it, the
                // whole-file path keeps the preview.
                let adobe_rgb = lightcraft_raw::embedded_preview_color_space(&buf) == Some(lightcraft_raw::PreviewColorSpace::AdobeRgb);
                // `extract` copies a TIFF's bytes to hand on its Exif block, so ask the cheap side
                // first: a JPEG that carries its own orientation needs nothing from the container.
                if lightcraft_meta::extract(&window).orientation.is_none()
                    && lightcraft_meta::extract(&header).orientation.is_none()
                    && lightcraft_meta::extract(&buf).orientation.is_some()
                {
                    return Ok(None);
                }
                let read = PreviewRead { read: loaded.total(), total, header_path: true, planned: true, adobe_rgb };
                // A plan-driven preview is the one that used to cost the whole file, so it is worth
                // saying what it cost now too.
                if lightcraft_pipeline::profiling() {
                    eprintln!("[profile] preview {path}: read {} of {} bytes (plan, window {})", read.read, total, window.len());
                }
                // The planner's buffer is the file's length, so give its pages back the way a decode
                // does (see `fs_pair_loader`).
                drop(buf);
                crate::memory::release();
                return Ok(Some((header, window, read)));
            }
            lightcraft_raw::sparse::HeaderPlan::Whole(_) => return Ok(None),
        }
    }
    Ok(None)
}

/// The file's first [`lightcraft_raw::PREVIEW_PREFIX`] bytes (or the whole file when it is shorter),
/// which is what every format [`lightcraft_raw::bounded_preview`] covers keeps its pointer in.
fn read_prefix(file: &mut std::fs::File, path: &str, total: u64) -> Result<Vec<u8>, String> {
    let len = lightcraft_raw::PREVIEW_PREFIX.min(usize::try_from(total).unwrap_or(usize::MAX));
    read_prefix_at(file, path, len as u64)
}

/// The file's first `len` bytes, read from the start.
fn read_prefix_at(file: &mut std::fs::File, path: &str, len: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = usize::try_from(len).map_err(|_| format!("{path}: file too large"))?;
    let mut prefix = vec![0u8; len];
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("{path}: {e}"))?;
    file.read_exact(&mut prefix).map_err(|e| format!("{path}: {e}"))?;
    Ok(prefix)
}

/// The end of a Fuji RAF's preview block, from the two big-endian u32s every RAF header carries at
/// 84 (offset) and 88 (length) — the same fields `lightcraft_raw`'s RAF reader uses, read here
/// without the rest of the file so the prefix can be made big enough first.
fn raf_preview_extent(prefix: &[u8]) -> Option<u64> {
    if !prefix.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let u32_at = |at: usize| prefix.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])).map(u64::from);
    let (offset, len) = (u32_at(84)?, u32_at(88)?);
    (offset > 0 && len > 0).then(|| offset.checked_add(len)).flatten()
}

/// The display-ready preview of a raw whose embedded JPEG was already located: the counterpart of
/// [`embedded_preview_srgb`] for [`preview_window`], which does not re-read the container.
/// `adobe_rgb`: the container declares the preview Adobe RGB ([`PreviewRead::adobe_rgb`]), applied
/// as [`decode_raw_preview`] applies it. `None` when the bytes are not a decodable image.
fn quick_image_srgb(header: &[u8], window: &[u8], adobe_rgb: bool, max_edge: usize) -> Option<lightcraft_raster::Rgba8> {
    let decoded = if adobe_rgb && window.starts_with(&[0xff, 0xd8]) {
        lightcraft_codecs::decode_jpeg_with_fallback(window, fit_box(max_edge), lightcraft_codecs::NamedSpace::AdobeRgb)
    } else {
        lightcraft_codecs::decode(window, fit_box(max_edge))
    };
    let mut d = decoded.ok()?;
    if d.image.width.max(d.image.height) > max_edge {
        d.image = fit(&d.image, max_edge, max_edge, Filter::Box);
        d.alpha = None;
    }
    // The JPEG's own EXIF orientation, else the raw's (from its header), as the whole-file path does
    Some(d.to_srgb8().oriented(preview_orientation(header, d.orientation)))
}

/// The filesystem-backed [`PairLoader`]: the file is read and decoded once, and developed as usual and from the photo's
/// denoised picture. It holds two pictures at once, so it counts for more at the memory gate than a plain load.
pub fn fs_pair_loader() -> PairLoader {
    Arc::new(|path: &str, max_edge: usize, spec: &DenoiseSpec| {
        let len = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        let weight = len * if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 4 } else { 10 };
        let gate = crate::memory::work_gate();
        let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let r = load_vec_pair(bytes, max_edge, spec);
        crate::memory::release();
        r
    })
}

/// Filesystem-backed hooks (native). On the web the host installs bytes-based hooks instead.
///
/// Their working memory is bounded by [`crate::memory::work_gate`]: background loads (grid
/// thumbnails, neighbour prefetch: [`crate::memory::in_background`]) and import probes wait while
/// too much is in flight, each counting its file's size times the expansion of decoding it;
/// interactive loads (the loupe, exports) never wait — they are counted, so background work
/// yields to them.
pub fn fs_hooks() -> (FileLoader, FileProbe) {
    let loader: FileLoader = Arc::new(|path: &str, max_edge: usize| fs_load(path, max_edge, |bytes| load_vec(bytes, max_edge)));
    let probe: FileProbe = Arc::new(|path: &str| {
        let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
        let _permit = crate::memory::work_gate().acquire(len);
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let mut info = probe_bytes(path, &bytes)?;
        info.file_modified = file_modified(path);
        Ok(info)
    });
    (loader, probe)
}

/// The filesystem [`ReducedLoader`]: [`load_vec_reduced`] of the file, gated like [`fs_hooks`]' loader.
pub fn fs_reduced_loader() -> ReducedLoader {
    Arc::new(|path: &str, max_edge: usize| fs_load(path, max_edge, |bytes| load_vec_reduced(bytes, max_edge)))
}

/// Read `path` and `load` it, waiting on [`crate::memory::work_gate`] as [`fs_hooks`] explains.
fn fs_load<T>(path: &str, max_edge: usize, load: impl FnOnce(Vec<u8>) -> Result<T, String>) -> Result<T, String> {
    let len = std::fs::metadata(path).map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX)).unwrap_or(0);
    let weight = len.saturating_mul(if max_edge <= crate::media::SourceLevel::Thumb.max_edge() { 3 } else { 6 });
    let gate = crate::memory::work_gate();
    let _permit = if crate::memory::is_background() { gate.acquire(weight) } else { gate.acquire_urgent(weight) };
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let r = load(bytes);
    // the file, the samples and the intermediate images are gone: give their pages back
    crate::memory::release();
    r
}

/// `path`'s modification time as ISO 8601 (see [`ProbeInfo::file_modified`]); `None` when the
/// filesystem doesn't say or the time is before 1970.
fn file_modified(path: &str) -> Option<String> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()?;
    Some(crate::import::civil(secs))
}

impl crate::Session {
    /// Install the filesystem file hooks (desktop, CLI, MCP headless), and on macOS the Apple Photos
    /// runner ([`crate::apple_photos::osascript`]; installed, not run).
    pub fn with_fs(mut self) -> Self {
        let (l, p) = fs_hooks();
        self.media.file_loader = Some(l);
        self.media.reduced_loader = Some(fs_reduced_loader());
        self.media.file_probe = Some(p);
        self.media.preview_loader = Some(fs_preview_loader());
        self.media.denoise.loader = Some(fs_pair_loader());
        #[cfg(target_os = "macos")]
        {
            self.apple_photos = Some(crate::apple_photos::osascript());
        }
        self
    }
}

#[cfg(test)]
pub(crate) mod tests;
