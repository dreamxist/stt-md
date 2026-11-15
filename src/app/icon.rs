//! The menubar glyph, cut out of the app icon at launch.

use tray_icon::Icon;

const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");
const TRAY_ICON_SIZE: u32 = 20;

pub fn build_idle_icon() -> Icon {
    match glyph_icon_from_png(APP_ICON_PNG, TRAY_ICON_SIZE) {
        Ok(icon) => icon,
        Err(e) => {
            eprintln!("[stt-md] failed to build tray icon from app icon: {e:?}");
            fallback_idle_icon()
        }
    }
}

/// Extracts the light glyph (waveform + text lines) from the app icon and
/// turns it into a monochrome template image: luminance → alpha, so the dark
/// tile background disappears and macOS tints it for light/dark menubars.
fn glyph_icon_from_png(png_bytes: &[u8], size: u32) -> anyhow::Result<Icon> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let bpp = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => anyhow::bail!("unsupported app-icon color type: {other:?}"),
    };
    anyhow::ensure!(info.bit_depth == png::BitDepth::Eight, "expected 8-bit png");

    // Per-source-pixel glyph alpha, then area-average downscale for smooth edges.
    let glyph_alpha = |x: usize, y: usize| -> f32 {
        let p = (y * w + x) * bpp;
        let (r, g, b) = (buf[p] as f32, buf[p + 1] as f32, buf[p + 2] as f32);
        let a = if bpp == 4 { buf[p + 3] as f32 / 255.0 } else { 1.0 };
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        // Tile background is dark navy (lum ≈ 20-40); glyph is near-white.
        (((lum - 60.0) / 80.0).clamp(0.0, 1.0)) * a
    };

    // Crop to the glyph's (square, centered) bounding box: the tile has a lot
    // of padding and the glyph would come out tiny at menubar size.
    let (mut bx0, mut bx1, mut by0, mut by1) = (w, 0usize, h, 0usize);
    for y in 0..h {
        for x in 0..w {
            if glyph_alpha(x, y) > 0.5 {
                bx0 = bx0.min(x);
                bx1 = bx1.max(x);
                by0 = by0.min(y);
                by1 = by1.max(y);
            }
        }
    }
    anyhow::ensure!(bx0 <= bx1 && by0 <= by1, "no glyph pixels found in app icon");
    let side = (bx1 - bx0 + 1).max(by1 - by0 + 1);
    let cx = (bx0 + bx1) / 2;
    let cy = (by0 + by1) / 2;
    let ox0 = cx.saturating_sub(side / 2) as isize;
    let oy0 = cy.saturating_sub(side / 2) as isize;

    let out_px = size as usize;
    let mut rgba = vec![0u8; out_px * out_px * 4];
    for oy in 0..out_px {
        let (y0, y1) = (oy * side / out_px, ((oy + 1) * side / out_px).max(oy * side / out_px + 1));
        for ox in 0..out_px {
            let (x0, x1) = (ox * side / out_px, ((ox + 1) * side / out_px).max(ox * side / out_px + 1));
            let mut acc = 0.0;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let (ix, iy) = (ox0 + sx as isize, oy0 + sy as isize);
                    if ix >= 0 && (ix as usize) < w && iy >= 0 && (iy as usize) < h {
                        acc += glyph_alpha(ix as usize, iy as usize);
                    }
                }
            }
            let avg = acc / ((y1 - y0) * (x1 - x0)) as f32;
            let idx = (oy * out_px + ox) * 4;
            rgba[idx + 3] = (avg * 255.0).round() as u8;
        }
    }
    Icon::from_rgba(rgba, size, size).map_err(|e| anyhow::anyhow!("bad rgba: {e}"))
}

fn fallback_idle_icon() -> Icon {
    // 20x20. "STT" en pixel art (5x7 por letra, separador 1px).
    let pattern: [&str; 20] = [
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "..####.#####.#####..",
        ".#........#.....#...",
        ".#........#.....#...",
        "..###.....#.....#...",
        ".....#....#.....#...",
        ".....#....#.....#...",
        ".####.....#.....#...",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
        "....................",
    ];
    rgba_from_pattern(&pattern)
}

fn rgba_from_pattern(pattern: &[&str]) -> Icon {
    let size = pattern.len() as u32;
    debug_assert!(pattern.iter().all(|r| r.chars().count() == size as usize));
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for (y, row) in pattern.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch == '#' {
                let idx = ((y as u32 * size + x as u32) * 4) as usize;
                rgba[idx + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("valid RGBA buffer")
}
