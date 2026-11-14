//! Encoding (PNG, JPEG), image token costs and `data:` URLs.

use base64::Engine as _;
use image::ImageEncoder as _;
use pageon_layout::Pixmap;

use super::Format;

/// Encodes a painted image. PNG keeps RGBA with fast compression (agents pay per pixel, not
/// per byte); JPEG is RGB (painted pages are opaque).
pub(crate) fn encode(px: &Pixmap, format: Format) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match format {
        Format::Png => image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(
            &px.data,
            px.width,
            px.height,
            image::ExtendedColorType::Rgba8,
        ),
        Format::Jpeg { quality } => {
            let rgb: Vec<u8> = px
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
                .write_image(&rgb, px.width, px.height, image::ExtendedColorType::Rgb8)
        }
    }
    .map_err(|e| e.to_string())?;
    Ok(out)
}

/// The cost of an image in `o200k` models (OpenAI's published high-detail rule): fit within
/// 2048×2048, then scale down so the shorter side is at most 768 px; 85 tokens plus 170 per
/// 512 px tile.
pub fn image_tokens(width: u32, height: u32) -> usize {
    let (mut w, mut h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let fit = (2048.0 / w.max(h)).min(1.0);
    w *= fit;
    h *= fit;
    let short = (768.0 / w.min(h)).min(1.0);
    w *= short;
    h *= short;
    let tiles = (w / 512.0).ceil() * (h / 512.0).ceil();
    85 + 170 * tiles as usize
}

/// The bytes of a `data:` URL (`data:[<mime>][;base64],<data>`).
pub(crate) fn data_url(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        let clean: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let clean = percent_decode(&clean);
        base64::engine::general_purpose::STANDARD
            .decode(clean.trim_end_matches('='))
            .or_else(|_| {
                base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.trim_end_matches('='))
            })
            .ok()
    } else {
        Some(percent_decode(data).into_bytes())
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o200k_costs() {
        assert_eq!(image_tokens(1024, 1024), 765); // OpenAI's documented example
        assert_eq!(image_tokens(2048, 4096), 1105); // ditto
        assert_eq!(image_tokens(1280, 720), 1105);
        assert_eq!(image_tokens(512, 512), 255);
    }

    #[test]
    fn data_urls() {
        assert_eq!(data_url("data:text/plain,a%20b").unwrap(), b"a b");
        assert_eq!(data_url("data:image/png;base64,aGk=").unwrap(), b"hi");
        assert!(data_url("https://x/").is_none());
    }
}
