use alloc::string::ToString;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Point, Size};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Alignment, Baseline, Text};
use embedded_graphics::Drawable;
use embedded_graphics::pixelcolor::BinaryColor;
use epd_waveshare::color::Black;
use epd_waveshare::color::White;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::U8g2TextStyle;

use super::draw_utils::{draw_loading_icon, draw_moon_icon, draw_wifi_status};
use super::render_data::CalendarRenderData;
use crate::display::EpdDisplay;
use crate::widgets::battery::draw_battery;
use crate::widgets::calendar::Calendar;
use crate::widgets::weather_icon::{WeatherKind, draw_weather_icon};

pub fn draw<D>(display: &mut D, data: &CalendarRenderData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = data.w;
    let h = data.h;

    // 状态图标（右上角）
    {
        let cy: i32 = 11;
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312>();
        if data.request_loading {
            draw_loading_icon(Point::new(w - 118, cy), display);
        }
        draw_wifi_status(Point::new(w - 100, cy), display);
        if let Some(percent) = data.battery_percent {
            let _ = draw_battery(percent, Point::new(w - 60, 4), Black, &font, display);
        }
    }

    let style = U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);
    let bottom_h: i32 = 45;

    // 天气数据（底部右侧，图标形式）
    if data.weather_synced {
        if let Some(weather) = data.weather {
            let bottom_y = h - bottom_h;
            let weather_x = 100;
            let weather_w = w - weather_x;
            let day_count = weather.daily.len() as i32;
            let col_w = weather_w / day_count.max(1);
            let font_small: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();
            let icon_size: u32 = 32;

            for (i, one) in weather.daily.iter().enumerate() {
                let cx = weather_x + col_w * i as i32 + col_w / 2;
                let kind = WeatherKind::from_code(one.code_day.as_str());
                let _ = draw_weather_icon(kind, Point::new(cx, bottom_y + icon_size as i32 / 2 + 2), icon_size, Black, display);

                let (_, date_part) = one.date.split_once('-').unwrap_or(("-", &one.date));
                let _ = font_small.render_aligned(
                    format_args!("{} {}", one.text_day.as_str(), date_part.replace("-", "/")),
                    Point::new(cx, bottom_y + 33),
                    VerticalPosition::Top,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                );
            }
        }
    } else {
        if data.wifi_connecting {
            let _ = Text::new("正在连接网络...", Point::new(0, 20), style.clone()).draw(display);
        } else {
            let _ = Text::new("正在同步天气...", Point::new(0, 20), style.clone()).draw(display);
        }
    }

    // 节假日同步状态
    if !data.holiday_synced {
        if data.wifi_connecting {
            let _ = Text::new("正在连接网络...", Point::new(0, 40), style.clone()).draw(display);
        } else {
            let _ = Text::new("正在同步节假日...", Point::new(0, 40), style.clone()).draw(display);
        }
    }

    // 日历 + 时钟
    if data.time_synced {
        if let Some(clock) = data.current_date {
            let hour = clock.hour();
            let minute = clock.minute();
            let time_str = format_args!("{:02}:{:02}", hour, minute).to_string();
            draw_clock(display, time_str.as_str());

            let calendar_rect = Rectangle::new(
                Point::new(0, 0),
                Size::new(w as u32, (h - bottom_h) as u32),
            );

            let year = clock.year();
            let month = clock.month();
            let today = clock.date();
            let mut calendar = Calendar::new(
                Point::default(), Size::default(),
                year, month, today, Black, White,
            );
            calendar.position = calendar_rect.top_left;
            calendar.size = calendar_rect.size;
            let _ = calendar.draw(display);
        }
    } else {
        let _ = Text::new("正在同步时间...", Point::new(0, 200), style).draw(display);
    }

    Ok(())
}

fn draw_clock<D>(display: &mut D, time: &str) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let dw: u32 = 18;
    let dh: u32 = 43;
    let sw: u32 = 4;
    let character_style = SevenSegmentStyleBuilder::new()
        .digit_size(Size::new(dw, dh))
        .segment_width(sw)
        .segment_color(Black)
        .build();

    let text_style = embedded_graphics::text::TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build();

    Text::with_text_style(
        time,
        Point::new(0, display.bounding_box().size.height as i32 - dh as i32 - 2),
        character_style,
        text_style,
    )
        .draw(display)?;

    Ok(())
}

pub fn sleep_renderer(display: &mut EpdDisplay) {
    let w = display.bounding_box().size.width as i32;
    let cy: i32 = 11;
    if crate::wifi::is_request_loading() {
        draw_loading_icon(Point::new(w - 118, cy), display);
    }
    draw_wifi_status(Point::new(w - 100, cy), display);
    draw_moon_icon(Point::new(w - 82, 4), display);
}
