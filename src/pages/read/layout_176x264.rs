use epd_waveshare::prelude::{Display, DisplayRotation};
use epd_waveshare::color::{Black, Color};
use embedded_graphics::prelude::Point;
use embedded_graphics::geometry::Dimensions;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use crate::display::EpdDisplay;

pub const DISPLAY_WIDTH: u32 = 176;
pub const DISPLAY_HEIGHT: u32 = 264;

pub const FONT_SIZE: u32 = 16;
pub const PROGRESS_AREA_HEIGHT: u32 = 20;

pub fn visual_width() -> u32 { DISPLAY_WIDTH }
pub fn visual_height() -> u32 { DISPLAY_HEIGHT }

/// Effective text area width — subtract one ZH char width to prevent the
/// last character on each line from being clipped (ZH_WIDTH overestimates
/// the actual glyph width by ~1 px, and the wrap logic adds the newline
/// *after* the overflow character).
pub fn text_width() -> u32 { visual_width() - 16 }
pub fn text_left_margin() -> i32 { ((visual_width() - text_width()) / 2) as i32 }

pub fn page_lines() -> u32 {
    (visual_height() - PROGRESS_AREA_HEIGHT) / FONT_SIZE - 1
}

pub fn current_rotation(flipped: bool) -> DisplayRotation {
    if flipped { DisplayRotation::Rotate180 } else { DisplayRotation::Rotate0 }
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
