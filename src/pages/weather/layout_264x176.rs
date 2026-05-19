use alloc::string::ToString;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Primitive};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::Black;
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::draw_utils::{draw_loading_icon, draw_moon_icon, draw_wifi_status, weekday_name};
use super::render_data::WeatherRenderData;
use crate::display::EpdDisplay;
use crate::model::lunar::Lunar;
use crate::widgets::battery::draw_battery;
use crate::widgets::temp_chart::{TempPoint, draw_temp_chart, draw_temp_labels};
use crate::widgets::weather_icon::{WeatherKind, draw_weather_icon};

pub fn draw<D>(display: &mut D, data: &WeatherRenderData) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = data.w;
    let h = data.h;

    let font_small: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();
    let font_medium: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312b>();

    // 未同步天气
    if !data.weather_synced {
        let style = U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);
        if data.wifi_connecting {
            let _ = Text::new("正在连接网络...", Point::new(0, 20), style.clone()).draw(display);
        } else {
            let _ = Text::new("正在同步天气...", Point::new(0, 20), style.clone()).draw(display);
        }
        if !data.holiday_synced {
            let _ = Text::new("正在同步节假日...", Point::new(0, 40), style).draw(display);
        }
        return Ok(());
    }

    let weather = match data.weather {
        Some(w) => w,
        None => {
            let style = U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);
            let _ = Text::new("无天气数据", Point::new(0, 20), style).draw(display);
            return Ok(());
        }
    };

    if weather.daily.is_empty() {
        return Ok(());
    }

    let daily = &weather.daily;
    let separator_style = PrimitiveStyleBuilder::new()
        .stroke_color(Black)
        .stroke_width(1)
        .build();

    let today = &daily[0];

    // ═══ is_small 布局 (w < 350) ═══
    let header_h: i32 = 34;
    let left_w = w / 3;
    let chart_margin_y: i32 = 6;

    // 顶栏：时钟 + 状态图标 + 电量
    if let Some(clock) = data.current_date {
        let time_str = format_args!("{:02}:{:02}", clock.hour(), clock.minute()).to_string();
        let _ = draw_clock(display, time_str.as_str(), Point::new(0, 2));
    }

    let bat_y: i32 = 2;
    let cy = bat_y + 7;
    if data.request_loading {
        draw_loading_icon(Point::new(w - 118, cy), display);
    }
    draw_wifi_status(Point::new(w - 100, cy), display);
    if let Some(percent) = data.battery_percent {
        let _ = draw_battery(percent, Point::new(w - 65, bat_y), Black, &font_small, display);
    }

    // 分隔线
    Line::new(Point::new(0, header_h), Point::new(w, header_h))
        .into_styled(separator_style.clone())
        .draw(display);

    // 左右分隔线
    Line::new(Point::new(left_w, header_h), Point::new(left_w, h))
        .into_styled(separator_style.clone())
        .draw(display);

    // 左面板：日期 + 天气详情 + 农历（is_small 版本）
    {
        let left_cx = left_w / 2;
        let mut y = header_h + 2;

        if let Some(clock) = data.current_date {
            let weekday = weekday_name(clock.weekday());
            let _ = font_small.render_aligned(
                format_args!("{}.{} {}", clock.month() as u8, clock.day(), weekday),
                Point::new(left_cx, y),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
        y += 14;

        let kind = WeatherKind::from_code(today.code_day.as_str());
        let _ = draw_weather_icon(kind, Point::new(left_cx, y + 11), 22, Black, display);
        y += 24;

        let _ = font_small.render_aligned(
            format_args!("{} {}/{}℃", today.text_day, today.high, today.low),
            Point::new(left_cx, y),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
        y += 14;

        if let Some(clock) = data.current_date {
            let lunar = Lunar::new(clock.year() as u16, clock.month() as u8);
            if let Some(lunar_day) = lunar.get_lunar_day(clock.day()) {
                let _ = font_small.render_aligned(
                    format_args!("{}{}", lunar_day.get_month_name(), lunar_day.get_day_name()),
                    Point::new(left_cx, y),
                    VerticalPosition::Top,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                );
            }
        }
    }

    // 右面板：白天图标 → 折线图 → 夜间图标
    let right_x = left_w + 1;
    let right_w = w - left_w - 1;
    let day_row_h: i32 = 36;
    let night_row_h: i32 = 36;

    let day_count = daily.len() as i32;
    let col_w = right_w / day_count.max(1);

    // 白天图标
    let day_icon_y = header_h;
    for (i, day) in daily.iter().enumerate() {
        let cx = right_x + col_w * i as i32 + col_w / 2;
        let kind = WeatherKind::from_code(day.code_day.as_str());
        let _ = draw_weather_icon(kind, Point::new(cx, day_icon_y + 16), 32, Black, display);
        let _ = font_small.render_aligned(
            day.text_day.as_str(),
            Point::new(cx, day_icon_y + 33),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
    }

    let sep_chart_top = day_icon_y + day_row_h;
    Line::new(Point::new(right_x, sep_chart_top), Point::new(w, sep_chart_top))
        .into_styled(separator_style.clone())
        .draw(display);

    // 温度折线图
    let night_row_y = h - night_row_h;
    let chart_y = sep_chart_top + chart_margin_y;
    let chart_h = night_row_y - sep_chart_top - chart_margin_y * 2;
    let chart_x = right_x + col_w / 2;
    let chart_w = col_w * (day_count - 1);

    let temp_points: heapless::Vec<TempPoint, 10> = daily.iter().map(|d| {
        let hi: i32 = d.high.as_str().parse().unwrap_or(0);
        let lo: i32 = d.low.as_str().parse().unwrap_or(0);
        TempPoint { label: "", high: hi, low: lo }
    }).collect();

    let _ = draw_temp_chart(
        Point::new(chart_x, chart_y),
        Size::new(chart_w as u32, chart_h as u32),
        &temp_points,
        Black,
        display,
    );

    let _ = draw_temp_labels(
        Point::new(chart_x, chart_y),
        Size::new(chart_w as u32, chart_h as u32),
        &temp_points,
        Black,
        &font_small,
        display,
    );

    Line::new(Point::new(right_x, night_row_y), Point::new(w, night_row_y))
        .into_styled(separator_style.clone())
        .draw(display);

    // 夜间图标 + 日期
    for (i, day) in daily.iter().enumerate() {
        let cx = right_x + col_w * i as i32 + col_w / 2;
        let kind = WeatherKind::from_code(day.code_night.as_str());
        let _ = draw_weather_icon(kind, Point::new(cx, night_row_y + 16), 32, Black, display);

        let (_, date_part) = day.date.split_once('-').unwrap_or(("-", &day.date));
        let _ = font_small.render_aligned(
            day.text_night.as_str(),
            Point::new(cx, night_row_y + 33),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
        let _ = font_small.render_aligned(
            format_args!("{}", date_part.replace("-", "/")),
            Point::new(cx, night_row_y + 46),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
    }

    Ok(())
}

fn draw_clock<D>(display: &mut D, time: &str, position: Point) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let dw: u32 = 14;
    let dh: u32 = 30;
    let sw: u32 = 3;
    let character_style = SevenSegmentStyleBuilder::new()
        .digit_size(Size::new(dw, dh))
        .segment_width(sw)
        .segment_color(Black)
        .build();

    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build();

    Text::with_text_style(time, position, character_style, text_style)
        .draw(display)?;

    Ok(())
}

pub fn sleep_renderer(display: &mut EpdDisplay) {
    let w = display.bounding_box().size.width as i32;
    let bat_y: i32 = 2;
    let cy = bat_y + 7;
    if crate::wifi::is_request_loading() {
        draw_loading_icon(Point::new(w - 118, cy), display);
    }
    draw_wifi_status(Point::new(w - 100, cy), display);
    draw_moon_icon(Point::new(w - 82, bat_y), display);
}
