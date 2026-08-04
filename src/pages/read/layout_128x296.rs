use epd_waveshare::prelude::{Display, DisplayRotation};
use epd_waveshare::color::{Black, Color};
use embedded_graphics::prelude::Point;
use embedded_graphics::geometry::Dimensions;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use core::sync::atomic::{AtomicU8, Ordering};

use crate::display::EpdDisplay;

pub const DISPLAY_WIDTH: u32 = 296;
pub const DISPLAY_HEIGHT: u32 = 128;

pub const FONT_SIZE: u32 = 16;
pub const PROGRESS_AREA_HEIGHT: u32 = 20;

/// 默认朝向旋转号：128x296 默认 Rotate90(竖屏)。
const DEFAULT_ROT_NUM: u8 = 1;
static ROTATE_STATE: AtomicU8 = AtomicU8::new(0);

pub fn set_rotation_state(r: u8) { ROTATE_STATE.store(r % 4, Ordering::Relaxed); }
fn cur_rot_num() -> u8 { (DEFAULT_ROT_NUM.wrapping_add(ROTATE_STATE.load(Ordering::Relaxed))) % 4 }
fn swapped() -> bool { cur_rot_num() % 2 == 1 }

pub fn visual_width() -> u32 { if swapped() { DISPLAY_HEIGHT } else { DISPLAY_WIDTH } }
pub fn visual_height() -> u32 { if swapped() { DISPLAY_WIDTH } else { DISPLAY_HEIGHT } }

pub fn text_width() -> u32 { visual_width() }
pub fn text_left_margin() -> i32 { 0 }

pub fn page_lines() -> u32 {
    (visual_height() - PROGRESS_AREA_HEIGHT) / FONT_SIZE - 1
}

pub fn current_rotation() -> DisplayRotation {
    match cur_rot_num() {
        0 => DisplayRotation::Rotate0,
        1 => DisplayRotation::Rotate90,
        2 => DisplayRotation::Rotate180,
        _ => DisplayRotation::Rotate270,
    }
}

pub fn sleep_renderer(display: &mut EpdDisplay) {
    display.clear_buffer(Color::White);
    let drawn = crate::flash_sleep::draw_sleep_image(display);
    if !drawn {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let font = font.with_ignore_unknown_chars(true);
        let center = Point::new(
            display.bounding_box().size.width as i32 / 2,
            display.bounding_box().size.height as i32 / 2,
        );
        let _ = font.render_aligned(
            "睡眠中",
            center,
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
    }
}
