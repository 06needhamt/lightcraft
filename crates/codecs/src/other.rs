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
    check_fillable(format, bytes, w, h)?;
    if u64::from(w).saturating_mul(u64::from(h)) <= HEADER_DECODE_MAX_PIXELS {
        let d = decode(bytes, format, &DecodeOptions::fit(64, 64))?;
        return Ok((d.source_width, d.source_height, d.orientation));
    }
    Ok((w, h, 1))
}

/// The decoder's allocation limit: the RGBA buffer of the largest image `opts` allows. It bounds the
/// decoders' own frame buffers only: `DynamicImage::from_decoder` allocates the canvas without
/// consulting it, so the canvas is bounded by `check_size` and [`check_fillable`] beforehand.
fn limits(opts: &DecodeOptions) -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(opts.max_pixels.saturating_mul(4));
    l
}

/// Refuses a canvas larger than the file's bytes can describe, before the decoder allocates it
/// (issue #2553). Uncompressed BMP rows are stored in full at 1 bit per pixel or more, so the file
/// holds at least one byte per 8 pixels. An RLE BMP has no such bound: an end-of-bitmap code leaves
/// the rest of the picture blank, so it is left to `check_size`. A GIF frame's pixels come from its
/// LZW data alone; a code expands to at most 4096 pixels and takes 12 bits once the table holds
/// that many (fewer bits only while its strings are shorter), so at most 2731 pixels per byte.
fn check_fillable(format: Format, bytes: &[u8], w: u32, h: u32) -> Result<()> {
    let (len, per_byte) = match format {
        Format::Bmp if bmp_is_rle(bytes) => return Ok(()),
        Format::Gif => (gif_image_data_len(bytes).unwrap_or(bytes.len()), 2_731),
        _ => (bytes.len(), 8),
    };
    if u64::from(w).saturating_mul(u64::from(h)) > (len as u64).saturating_mul(per_byte) {
        return Err(Error::Malformed(format, format!("a {w} × {h} canvas is far larger than its {len} bytes of pixel data can hold")));
    }
    Ok(())
}

/// Whether a BMP's pixels are run-length encoded (RLE8 or RLE4, in a header of 40 bytes or more).
fn bmp_is_rle(bytes: &[u8]) -> bool {
    let u32_at = |i: usize| bytes.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    u32_at(14).is_some_and(|size| size >= 40) && matches!(u32_at(30), Some(1 | 2))
}

/// The bytes of LZW data present in a GIF's first frame; its palettes, comments and other
/// extensions describe no pixels. `None` when the blocks before the frame can't be walked.
fn gif_image_data_len(bytes: &[u8]) -> Option<usize> {
    let table = |flags: u8| if flags & 0x80 != 0 { 3usize << ((flags & 7) + 1) } else { 0 };
    let mut pos = 13 + table(*bytes.get(10)?);
    loop {
        match *bytes.get(pos)? {
            // extension: introducer, label, sub-blocks
            0x21 => pos = sub_blocks(bytes, pos.saturating_add(2)).1,
            // image descriptor (10 bytes), local colour table, LZW minimum code size, data sub-blocks
            0x2C => {
                let start = pos.saturating_add(10).saturating_add(table(*bytes.get(pos.saturating_add(9))?)).saturating_add(1);
                return Some(sub_blocks(bytes, start).0);
            }
            _ => return None,
        }
    }
}

/// The GIF sub-blocks starting at `pos`: the data bytes present, and the position after them.
fn sub_blocks(bytes: &[u8], mut pos: usize) -> (usize, usize) {
    let mut data = 0usize;
    while let Some(&n) = bytes.get(pos) {
        let start = pos.saturating_add(1);
        if n == 0 {
            return (data, start);
        }
        let end = start.saturating_add(usize::from(n));
        data = data.saturating_add(end.min(bytes.len()).saturating_sub(start));
        pos = end;
    }
    (data, pos)
}

pub(crate) fn decode(bytes: &[u8], format: Format, opts: &DecodeOptions) -> Result<Decoded> {
    let err = |e: image::ImageError| Error::Malformed(format, e.to_string());
    let cursor = std::io::Cursor::new(bytes);
    let (img, icc) = match format {
        Format::Gif => {
            let mut d = image::codecs::gif::GifDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes, w, h)?;
            d.set_limits(limits(opts)).map_err(err)?;
            let icc = d.icc_profile().ok().flatten();
            (DynamicImage::from_decoder(d).map_err(err)?, icc)
        }
        Format::Bmp => {
            let mut d = image::codecs::bmp::BmpDecoder::new(cursor).map_err(err)?;
            let (w, h) = d.dimensions();
            check_size(format, w as u64, h as u64, opts)?;
            check_fillable(format, bytes, w, h)?;
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
