use alloc::vec::Vec as AllocVec;
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::Point;
use embedded_graphics::{Drawable, Pixel};
use esp_println::println;
use crate::storage::{read_flash, write_flash};

const STORAGE_OFFSET: u32 = 0x310000;
const SLEEP_IMAGE_MAGIC: u32 = 0x534C4550;
const HEADER_SIZE: usize = 8;

const SLEEP_IMG_W: u32 = 300;
const SLEEP_IMG_H: u32 = 400;
const ROW_BYTES: usize = (SLEEP_IMG_W as usize + 7) / 8;

const DEFAULT_SLEEP_PIXELS: &[u8; ROW_BYTES * SLEEP_IMG_H as usize] =
    include_bytes!("../files/sleep_default.bin");

pub fn has_sleep_image() -> bool {
    let mut buf = [0u8; 4];
    if read_flash(STORAGE_OFFSET, &mut buf).is_err() {
        return false;
    }
    u32::from_le_bytes(buf) == SLEEP_IMAGE_MAGIC
}

pub fn get_sleep_image_size() -> Option<u32> {
    let mut header = [0u8; 8];
    read_flash(STORAGE_OFFSET, &mut header).ok()?;
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if magic != SLEEP_IMAGE_MAGIC {
        return None;
    }
    Some(u32::from_le_bytes([header[4], header[5], header[6], header[7]]))
}

pub fn delete_sleep_image() -> Result<(), &'static str> {
    let zero = [0u8; 8];
    write_flash(STORAGE_OFFSET, &zero).map_err(|_| "flash write failed")
}

/// Convert raw BMP data to packed 1-bit pixel data scaled to target dimensions.
/// Returns None if the BMP is invalid or unsupported.
pub fn bmp_to_pixels(raw_bmp: &[u8], target_w: u32, target_h: u32) -> Option<AllocVec<u8>> {
    if raw_bmp.len() < 54 || raw_bmp[0] != b'B' || raw_bmp[1] != b'M' {
        return None;
    }

    let pixel_offset = u32::from_le_bytes([raw_bmp[10], raw_bmp[11], raw_bmp[12], raw_bmp[13]]) as usize;
    let bmp_w = u32::from_le_bytes([raw_bmp[18], raw_bmp[19], raw_bmp[20], raw_bmp[21]]);
    let height_raw = i32::from_le_bytes([raw_bmp[22], raw_bmp[23], raw_bmp[24], raw_bmp[25]]);
    let bpp = u16::from_le_bytes([raw_bmp[28], raw_bmp[29]]) as u32;
    let top_down = height_raw < 0;
    let bmp_h = height_raw.unsigned_abs();

    if bmp_w == 0 || bmp_h == 0 || (bpp != 1 && bpp != 24 && bpp != 32) {
        return None;
    }

    let src_row_bytes = match bpp {
        1 => (bmp_w + 7) / 8,
        24 => bmp_w * 3,
        32 => bmp_w * 4,
        _ => return None,
    };
    let src_row_stride = ((src_row_bytes + 3) & !3u32) as usize;
    let row_bytes = (target_w as usize + 7) / 8;

    let total_size = row_bytes * target_h as usize;
    let mut pixels = AllocVec::with_capacity(total_size);
    pixels.resize(total_size, 0);

    for dy in 0..target_h {
        let src_y = dy as u32 * bmp_h / target_h;
        let src_y = if top_down { src_y } else { bmp_h - 1 - src_y };
        let row_start = pixel_offset + src_y as usize * src_row_stride;

        for dx in 0..target_w {
            let sx = dx as u32 * bmp_w / target_w;
            let is_black = match bpp {
                1 => {
                    let byte_idx = row_start + sx as usize / 8;
                    let bit_idx = 7 - (sx % 8);
                    byte_idx < raw_bmp.len() && (raw_bmp[byte_idx] >> bit_idx) & 1 == 0
                }
                24 => {
                    let px = row_start + sx as usize * 3;
                    if px + 2 < raw_bmp.len() {
                        let (b, g, r) = (raw_bmp[px] as u32, raw_bmp[px + 1] as u32, raw_bmp[px + 2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else {
                        false
                    }
                }
                32 => {
                    let px = row_start + sx as usize * 4;
                    if px + 2 < raw_bmp.len() {
                        let (b, g, r) = (raw_bmp[px] as u32, raw_bmp[px + 1] as u32, raw_bmp[px + 2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if is_black {
                let row_offset = dy as usize * row_bytes;
                let idx = dx as usize;
                pixels[row_offset + idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
    }

    Some(pixels)
}

/// Convert raw BMP data to packed 1-bit pixels, writing directly into a caller-provided buffer.
/// No heap allocation. Returns false if BMP is invalid or buffer is wrong size.
pub fn bmp_to_pixels_buf(raw_bmp: &[u8], target_w: u32, target_h: u32, out: &mut [u8]) -> bool {
    let row_bytes = (target_w as usize + 7) / 8;
    let total_size = row_bytes * target_h as usize;
    if out.len() != total_size { return false; }

    if raw_bmp.len() < 54 || raw_bmp[0] != b'B' || raw_bmp[1] != b'M' {
        return false;
    }

    let pixel_offset = u32::from_le_bytes([raw_bmp[10], raw_bmp[11], raw_bmp[12], raw_bmp[13]]) as usize;
    let bmp_w = u32::from_le_bytes([raw_bmp[18], raw_bmp[19], raw_bmp[20], raw_bmp[21]]);
    let height_raw = i32::from_le_bytes([raw_bmp[22], raw_bmp[23], raw_bmp[24], raw_bmp[25]]);
    let bpp = u16::from_le_bytes([raw_bmp[28], raw_bmp[29]]) as u32;
    let top_down = height_raw < 0;
    let bmp_h = height_raw.unsigned_abs();

    if bmp_w == 0 || bmp_h == 0 || (bpp != 1 && bpp != 24 && bpp != 32) {
        return false;
    }

    let src_row_bytes = match bpp {
        1 => (bmp_w + 7) / 8,
        24 => bmp_w * 3,
        32 => bmp_w * 4,
        _ => return false,
    };
    let src_row_stride = ((src_row_bytes + 3) & !3u32) as usize;

    for b in out.iter_mut() { *b = 0; }

    for dy in 0..target_h {
        let src_y = dy as u32 * bmp_h / target_h;
        let src_y = if top_down { src_y } else { bmp_h - 1 - src_y };
        let row_start = pixel_offset + src_y as usize * src_row_stride;

        for dx in 0..target_w {
            let sx = dx as u32 * bmp_w / target_w;
            let is_black = match bpp {
                1 => {
                    let byte_idx = row_start + sx as usize / 8;
                    let bit_idx = 7 - (sx % 8);
                    byte_idx < raw_bmp.len() && (raw_bmp[byte_idx] >> bit_idx) & 1 == 0
                }
                24 => {
                    let px = row_start + sx as usize * 3;
                    if px + 2 < raw_bmp.len() {
                        let (b, g, r) = (raw_bmp[px] as u32, raw_bmp[px + 1] as u32, raw_bmp[px + 2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else {
                        false
                    }
                }
                32 => {
                    let px = row_start + sx as usize * 4;
                    if px + 2 < raw_bmp.len() {
                        let (b, g, r) = (raw_bmp[px] as u32, raw_bmp[px + 1] as u32, raw_bmp[px + 2] as u32);
                        (r * 299 + g * 587 + b * 114) / 1000 < 128
                    } else {
                        false
                    }
                }
                _ => false,
            };
            if is_black {
                let row_offset = dy as usize * row_bytes;
                let idx = dx as usize;
                out[row_offset + idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
    }

    true
}

/// Parsed BMP header info for streaming operations.
pub struct BmpInfo {
    pub pixel_offset: usize,
    pub bmp_w: u32,
    pub bmp_h: u32,
    pub bpp: u32,
    pub top_down: bool,
    pub src_row_stride: usize,
}

impl BmpInfo {
    pub fn parse(header: &[u8]) -> Option<Self> {
        if header.len() < 54 || header[0] != b'B' || header[1] != b'M' {
            return None;
        }
        let pixel_offset = u32::from_le_bytes([header[10], header[11], header[12], header[13]]) as usize;
        let bmp_w = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
        let height_raw = i32::from_le_bytes([header[22], header[23], header[24], header[25]]);
        let bpp = u16::from_le_bytes([header[28], header[29]]) as u32;
        let top_down = height_raw < 0;
        let bmp_h = height_raw.unsigned_abs();
        if bmp_w == 0 || bmp_h == 0 || (bpp != 1 && bpp != 24 && bpp != 32) {
            return None;
        }
        let src_row_bytes = match bpp {
            1 => (bmp_w + 7) / 8,
            24 => bmp_w * 3,
            32 => bmp_w * 4,
            _ => return None,
        };
        let src_row_stride = ((src_row_bytes + 3) & !3u32) as usize;
        Some(Self { pixel_offset, bmp_w, bmp_h, bpp, top_down, src_row_stride })
    }
}

/// Convert one BMP source row to packed 1-bit pixels for a target row.
/// out must be at least (target_w + 7) / 8 bytes.
pub fn convert_bmp_row(src_row: &[u8], bpp: u32, bmp_w: u32, target_w: u32, out: &mut [u8]) {
    let row_bytes = (target_w as usize + 7) / 8;
    for b in out[..row_bytes].iter_mut() { *b = 0; }
    for dx in 0..target_w {
        let sx = dx * bmp_w / target_w;
        let is_black = match bpp {
            1 => {
                let byte_idx = sx as usize / 8;
                let bit_idx = 7 - (sx % 8);
                byte_idx < src_row.len() && (src_row[byte_idx] >> bit_idx) & 1 == 0
            }
            24 => {
                let px = sx as usize * 3;
                if px + 2 < src_row.len() {
                    let (b, g, r) = (src_row[px] as u32, src_row[px+1] as u32, src_row[px+2] as u32);
                    (r * 299 + g * 587 + b * 114) / 1000 < 128
                } else { false }
            }
            32 => {
                let px = sx as usize * 4;
                if px + 2 < src_row.len() {
                    let (b, g, r) = (src_row[px] as u32, src_row[px+1] as u32, src_row[px+2] as u32);
                    (r * 299 + g * 587 + b * 114) / 1000 < 128
                } else { false }
            }
            _ => false,
        };
        if is_black {
            out[dx as usize / 8] |= 1 << (7 - (dx as usize % 8));
        }
    }
}

/// Draw a packed 1-bit pixel row to the display at the given y position.
pub fn draw_pixel_row<D>(display: &mut D, row: &[u8], y: i32, width: u32)
where D: DrawTarget<Color = BinaryColor>
{
    for x in 0..width {
        let byte_idx = x as usize / 8;
        let bit_idx = 7 - (x as usize % 8);
        if (row[byte_idx] >> bit_idx) & 1 != 0 {
            let _ = Pixel(Point::new(x as i32, y), BinaryColor::On).draw(display);
        }
    }
}

/// Write one pixel row to flash at the given row index.
pub fn write_sleep_pixel_row(row_index: u32, pixel_row: &[u8]) -> Result<(), &'static str> {
    let offset = STORAGE_OFFSET + HEADER_SIZE as u32 + row_index * ROW_BYTES as u32;
    write_flash(offset, pixel_row).map_err(|_| "flash write failed")
}

pub fn begin_sleep_image_write() -> Result<(), &'static str> {
    let zero_header = [0u8; 8];
    write_flash(STORAGE_OFFSET, &zero_header).map_err(|_| "flash write failed")
}

pub fn finish_sleep_image_write() -> Result<(), &'static str> {
    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&SLEEP_IMAGE_MAGIC.to_le_bytes());
    let total_size = (ROW_BYTES * SLEEP_IMG_H as usize) as u32;
    header[4..8].copy_from_slice(&total_size.to_le_bytes());
    write_flash(STORAGE_OFFSET, &header).map_err(|_| "flash write header failed")?;
    println!("flash sleep image saved (streaming), {} bytes", total_size);
    Ok(())
}

/// Draw packed 1-bit pixel data to a display.
/// pixels format: row-by-row, MSB first, 1 = black pixel.
pub fn draw_pixels<D>(display: &mut D, pixels: &[u8], width: u32, height: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let row_bytes = (width as usize + 7) / 8;
    for y in 0..height {
        let row_start = y as usize * row_bytes;
        let row = &pixels[row_start..row_start + row_bytes];
        draw_pixel_row(display, row, y as i32, width);
    }
}

pub fn save_sleep_image(raw_bmp: &[u8]) -> Result<(), &'static str> {
    let pixels = bmp_to_pixels(raw_bmp, SLEEP_IMG_W, SLEEP_IMG_H)
        .ok_or("invalid BMP")?;

    let total_size = pixels.len();

    // Write invalid magic first (crash safety)
    let zero_header = [0u8; 8];
    write_flash(STORAGE_OFFSET, &zero_header).map_err(|_| "flash write failed")?;

    // Write pixel data
    write_flash(STORAGE_OFFSET + HEADER_SIZE as u32, &pixels)
        .map_err(|_| "flash write pixels failed")?;

    // Write valid header
    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&SLEEP_IMAGE_MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&(total_size as u32).to_le_bytes());
    write_flash(STORAGE_OFFSET, &header).map_err(|_| "flash write header failed")?;

    println!("flash sleep image saved, {} bytes", total_size);
    Ok(())
}

pub fn draw_sleep_image<D>(display: &mut D) -> bool
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut header = [0u8; 8];
    let has_flash_image = read_flash(STORAGE_OFFSET, &mut header).is_ok()
        && u32::from_le_bytes([header[0], header[1], header[2], header[3]]) == SLEEP_IMAGE_MAGIC;

    if has_flash_image {
        let mut row_buf = [0u8; ROW_BYTES];
        for y in 0..SLEEP_IMG_H {
            let offset = STORAGE_OFFSET + HEADER_SIZE as u32 + y as u32 * ROW_BYTES as u32;
            if read_flash(offset, &mut row_buf).is_err() {
                return false;
            }
            for x in 0..SLEEP_IMG_W {
                let byte_idx = x as usize / 8;
                let bit_idx = 7 - (x as usize % 8);
                if (row_buf[byte_idx] >> bit_idx) & 1 != 0 {
                    let _ = Pixel(Point::new(x as i32, y as i32), BinaryColor::On).draw(display);
                }
            }
        }
    } else {
        draw_pixels(display, DEFAULT_SLEEP_PIXELS, SLEEP_IMG_W, SLEEP_IMG_H);
    }
    true
}
