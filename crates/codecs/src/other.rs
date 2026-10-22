//! GIF (first frame) and BMP via the `image` crate's pure-Rust decoders.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use image::{DynamicImage, ImageDecoder};

/// Canvases up to this many pixels are still decoded by [`header`] (at most 16 MB), so a truncated
/// small file is refused at import as before.
const HEADER_DECODE_MAX_PIXELS: u64 = 4_000_000;

/// Stored size for the import probe (issue #2553). The probe used to decode these in full, and a
/// few-dozen-byte file declaring a gigapixel canvas allocated gigabytes. The size now comes from the
/// header and must be one the file's bytes can fill; only a canvas small enough to be cheap is
/// decoded (to catch truncation). Orientation is always 1: neither format carries EXIF here.
pub(crate) fn header(bytes: &[u8], format: Format) -> Result<(u32, u32, u16)> {
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (w, h) = match format {
        Format::Gif => image::codecs::gif::GifDecoder::new(cursor).map_err(err)?.dimensions(),
        Format::Bmp => image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?.dimensions(),
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    if w == 0 || h == 0 {
        return Err(Error::Malformed(format, "zero dimension".into()));
    }
    check_fillable(format, bytes.len(), w, h)?;
    if u64::from(w).saturating_mul(u64::from(h)) <= HEADER_DECODE_MAX_PIXELS {
        let d = decode(bytes, format, &DecodeOptions::fit(64, 64))?;
        return Ok((d.source_width, d.source_height, d.orientation));
    }
    Ok((w, h, 1))
}

/// The decoder's allocation limit: the RGBA buffer of the largest image `opts` allows, so an
/// oversized canvas is an error from the decoder instead of an allocation attempt.
fn limits(opts: &DecodeOptions) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(opts.max_pixels.saturating_mul(4));
    l
}

/// The most pixels `len` bytes of pixel data can describe: BMP stores at least 1 bit per pixel and
/// its RLE at most 255 pixels per 2-byte run (128 per byte covers both); a GIF LZW code takes at
/// least 3 bits and expands to at most 4096 pixels (11,000 per byte). The decoders allocate the
/// whole canvas before reading any pixel data, so a header declaring far more than the file can
/// fill is refused first (issue #2553).
fn fillable(format: Format, len: usize) -> u64 {
    let per_byte = if format == Format::Gif { 11_000 } else { 128 };
    (len as u64).saturating_mul(per_byte)
}

fn check_fillable(format: Format, len: usize, w: u32, h: u32) -> Result<()> {
    if u64::from(w).saturating_mul(u64::from(h)) > fillable(format, len) {
        return Err(Error::Malformed(format, format!("a {w} × {h} canvas is far larger than its {len} bytes can hold")));
    }
    Ok(())
}

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (img, icc) = match format {
        Format::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes.len(), w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        Format::Bmp => {
            let mut d = image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes.len(), w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        _ => return Err(Error::Unsupported(format, "not handled by this decoder")),
    };
    let (w, h) = (img.width(), img.height());
    let alpha = img.color().has_alpha();
    let gray = !img.color().has_color();
    let (model, alpha, buf) = match (gray, alpha) {
        (true, false) => (Model::Gray, false, img.into_luma8().into_raw()),
        (true, true) => (Model::Gray, true, img.into_luma_alpha8().into_raw()),
        (false, false) => (Model::Rgb, false, img.into_rgb8().into_raw()),
        (false, true) => (Model::Rgb, true, img.into_rgba8().into_raw()),
    };
    let raw = Raw { width: w as usize, height: h as usize, model, alpha, premultiplied: false, buf: Buf::U8(buf), bit_depth: 8 };
    finish(format, raw, Meta { icc, ..Default::default() }, (w, h), opts)
}
