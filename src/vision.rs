use std::io::Cursor;

use image::{ImageFormat, Rgb, RgbImage};

use crate::dom::ActionableElement;
use crate::error::{Result, UicError};

/// Minimal 5x7 bitmap glyphs for digits 0-9.
const DIGITS_5X7: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111], // 2
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110], // 5
    [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110], // 9
];

/// Draws crisp Set-of-Mark bounding boxes and numbered badges onto a JPEG screenshot in memory.
/// Zero DOM mutation on the live page.
pub fn annotate_screenshot_jpeg(
    raw_jpeg: &[u8],
    elements: &[ActionableElement],
    viewport_width: f64,
    viewport_height: f64,
    out_quality: u8,
) -> Result<Vec<u8>> {
    let mut img = image::load_from_memory(raw_jpeg)
        .map_err(|e| UicError::Vision(format!("failed to decode screenshot: {e}")))?
        .to_rgb8();

    let (img_w, img_h) = img.dimensions();
    let scale_x = if viewport_width > 0.0 {
        img_w as f64 / viewport_width
    } else {
        1.0
    };
    let scale_y = if viewport_height > 0.0 {
        img_h as f64 / viewport_height
    } else {
        1.0
    };

    let border_color = Rgb([225, 29, 72]); // crimson #E11D48
    let badge_bg = Rgb([225, 29, 72]);
    let badge_fg = Rgb([255, 255, 255]);

    for el in elements {
        let max_x = (img_w as i32 - 1).max(0);
        let max_y = (img_h as i32 - 1).max(0);
        let x0 = ((el.rect.x * scale_x).round() as i32).clamp(0, max_x);
        let y0 = ((el.rect.y * scale_y).round() as i32).clamp(0, max_y);
        let x1 = (((el.rect.x + el.rect.width) * scale_x).round() as i32).clamp(x0, max_x);
        let y1 = (((el.rect.y + el.rect.height) * scale_y).round() as i32).clamp(y0, max_y);

        // Subtle 1px outline around actionable element
        draw_rect_outline(&mut img, x0, y0, x1, y1, border_color);

        // Numbered badge at top-left corner of the element
        let label = el.index.to_string();
        let glyph_scale = 2i32;
        let char_w = 5 * glyph_scale;
        let char_h = 7 * glyph_scale;
        let gap = glyph_scale;
        let pad = 3i32;
        let text_w = (label.len() as i32) * char_w + ((label.len() as i32 - 1).max(0)) * gap;
        let box_w = text_w + pad * 2;
        let box_h = char_h + pad * 2;

        let bx = x0.min((img_w as i32 - box_w).max(0));
        let by = (y0 - box_h + 4).max(0).min((img_h as i32 - box_h).max(0));

        fill_rect(&mut img, bx, by, bx + box_w, by + box_h, badge_bg);
        draw_rect_outline(&mut img, bx, by, bx + box_w, by + box_h, badge_fg);

        let mut cursor_x = bx + pad;
        for ch in label.bytes() {
            if ch.is_ascii_digit() {
                let d = (ch - b'0') as usize;
                draw_digit(&mut img, cursor_x, by + pad, d, glyph_scale, badge_fg);
                cursor_x += char_w + gap;
            }
        }
    }

    encode_rgb_jpeg(&img, out_quality)
}

pub fn encode_rgb_jpeg(img: &RgbImage, out_quality: u8) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::with_capacity(65536));
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, out_quality);
    encoder
        .encode_image(img)
        .map_err(|e| UicError::Vision(format!("failed to encode JPEG: {e}")))?;
    let _ = ImageFormat::Jpeg;
    Ok(out.into_inner())
}

pub type GridOverlay = (Vec<u8>, Vec<(usize, usize)>);

/// Draws a `cols x rows` visual targeting grid with numbered red badges `[1]..[N]` onto an image.
/// Returns the encoded JPEG and the `(col, row)` coordinate list for each cell index.
pub fn draw_grid_overlay_jpeg(
    src: &RgbImage,
    cols: usize,
    rows: usize,
    out_quality: u8,
) -> Result<GridOverlay> {
    let mut img = src.clone();
    let (img_w, img_h) = img.dimensions();
    let grid_color = Rgb([225, 29, 72]);
    let badge_bg = Rgb([225, 29, 72]);
    let badge_fg = Rgb([255, 255, 255]);

    let cell_w = img_w as f64 / cols as f64;
    let cell_h = img_h as f64 / rows as f64;

    // Draw grid lines
    for c in 1..cols {
        let x = (c as f64 * cell_w).round() as i32;
        for y in 0..img_h as i32 {
            set_px(&mut img, x, y, grid_color);
        }
    }
    for r in 1..rows {
        let y = (r as f64 * cell_h).round() as i32;
        for x in 0..img_w as i32 {
            set_px(&mut img, x, y, grid_color);
        }
    }

    let mut cells = Vec::with_capacity(cols * rows);
    let glyph_scale = 2i32;
    let char_w = 5 * glyph_scale;
    let char_h = 7 * glyph_scale;
    let gap = glyph_scale;
    let pad = 3i32;

    for r in 0..rows {
        for c in 0..cols {
            cells.push((c, r));
            let idx = cells.len();
            let label = idx.to_string();
            let text_w = (label.len() as i32) * char_w + ((label.len() as i32 - 1).max(0)) * gap;
            let box_w = text_w + pad * 2;
            let box_h = char_h + pad * 2;

            let bx = ((c as f64 * cell_w).round() as i32 + 3).min((img_w as i32 - box_w).max(0));
            let by = ((r as f64 * cell_h).round() as i32 + 3).min((img_h as i32 - box_h).max(0));

            fill_rect(&mut img, bx, by, bx + box_w, by + box_h, badge_bg);
            draw_rect_outline(&mut img, bx, by, bx + box_w, by + box_h, badge_fg);

            let mut cursor_x = bx + pad;
            for ch in label.bytes() {
                if ch.is_ascii_digit() {
                    let d = (ch - b'0') as usize;
                    draw_digit(&mut img, cursor_x, by + pad, d, glyph_scale, badge_fg);
                    cursor_x += char_w + gap;
                }
            }
        }
    }

    Ok((encode_rgb_jpeg(&img, out_quality)?, cells))
}

fn set_px(img: &mut RgbImage, x: i32, y: i32, color: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, color);
    }
}

fn fill_rect(img: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb<u8>) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            set_px(img, x, y, color);
        }
    }
}

fn draw_rect_outline(img: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb<u8>) {
    for x in x0..=x1 {
        set_px(img, x, y0, color);
        set_px(img, x, y1, color);
    }
    for y in y0..=y1 {
        set_px(img, x0, y, color);
        set_px(img, x1, y, color);
    }
}

fn draw_digit(img: &mut RgbImage, x: i32, y: i32, digit: usize, scale: i32, color: Rgb<u8>) {
    let rows = DIGITS_5X7[digit];
    for (r, bits) in rows.iter().enumerate() {
        for c in 0..5 {
            if (bits >> (4 - c)) & 1 == 1 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        set_px(img, x + c * scale + dx, y + (r as i32) * scale + dy, color);
                    }
                }
            }
        }
    }
}
