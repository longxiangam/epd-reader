use alloc::string::ToString;
use core::fmt::Write;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Primitive, Transform};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::Black;
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use super::draw_utils::weekday_name;
use crate::widgets::draw_icon::{draw_loading_icon, draw_moon_icon, draw_wifi_status};
use super::render_data::WeatherRenderData;
use crate::display::EpdDisplay;
use crate::model::lunar::{Lunar, get_solar_term, get_zodiac, solar_term_day};
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
    let font_medium: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>();

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

    // ═══ 全尺寸布局 (w >= 350) ═══
    let header_h: i32 = 36;
    let left_w = w / 3;
    let chart_margin_y: i32 = 12;

    // 顶栏左上角：当前城市（大） + 天气最后更新时间（小）
    let _ = font_medium.render_aligned(
        weather.location.name.as_str(),
        Point::new(4, 1),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );
    {
        // 显示“设备最后一次请求天气接口的时间”（WEATHER_SYNC_SECOND，UTC+8 本地时间）。
        // 不使用接口返回的 last_update：心知天气 daily 预报固定每天 08:00 生成，
        // 会一直显示 08:00，无法反映设备真实抓取时刻。
        let sync_sec = data.weather_sync_second;
        let mut hhmm: heapless::String<5> = heapless::String::new();
        if sync_sec > 1577836800 {
            // 大于 2020-01-01 视为有效；设备时区固定 UTC+8
            let local = sync_sec + 8 * 3600;
            let _ = write!(hhmm, "{:02}:{:02}", (local / 3600) % 24, (local / 60) % 60);
        } else {
            let _ = hhmm.push_str("--:--");
        }
        let _ = font_small.render_aligned(
            format_args!("更新 {}", hhmm),
            Point::new(4, 20),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(Black),
            display,
        );
    }

    let bat_y: i32 = 10;
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

    Line::new(Point::new(left_w, header_h), Point::new(left_w, h))
        .into_styled(separator_style.clone())
        .draw(display);

    // 左面板：日期 + 天气详情 + 农历（完整版本）
    {
        let left_cx = left_w / 2;
        let mut y = header_h + 4;

        let kind = WeatherKind::from_code(today.code_day.as_str());
        let _ = draw_weather_icon(kind, Point::new(left_cx, y + 16), 32, Black, display);
        y += 36;

        let _ = font_medium.render_aligned(
            today.text_day.as_str(),
            Point::new(left_cx, y),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
        y += 20;

        let _ = font_medium.render_aligned(
            format_args!("{}/{}℃", today.high, today.low),
            Point::new(left_cx, y),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
        y += 22;

        Line::new(Point::new(4, y), Point::new(left_w - 4, y))
            .into_styled(separator_style.clone())
            .draw(display);
        y += 4;

        let _ = font_small.render_aligned(
            format_args!("湿度: {}%", today.humidity),
            Point::new(6, y),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(Black),
            display,
        );
        y += 16;

        let _ = font_small.render_aligned(
            format_args!("{}{}级", today.wind_direction, today.wind_scale),
            Point::new(6, y),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(Black),
            display,
        );
        y += 16;

        let _ = font_small.render_aligned(
            format_args!("降水: {}mm", today.rainfall),
            Point::new(6, y),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(Black),
            display,
        );
        y += 20;

        Line::new(Point::new(4, y), Point::new(left_w - 4, y))
            .into_styled(separator_style.clone())
            .draw(display);

        if let Some(clock) = data.current_date {
            // 时间区域底部对齐：把时钟 + 三行文字整块下移到面板底部，消除下方空白。
            // 块高 = 时钟段(56) + 年月日(16) + 农历(16) + 星座节气(16)
            y = h - (56 + 16 + 16 + 16) - 4;
            // 时间（放大）
            let time_str = format_args!("{:02}:{:02}", clock.hour(), clock.minute()).to_string();
            let _ = draw_clock(display, time_str.as_str(), left_cx, y);
            y += 56;
            // 年月日 星期
            let weekday = weekday_name(clock.weekday());
            let _ = font_small.render_aligned(
                format_args!("{}.{}.{} {}", clock.year(), clock.month() as u8, clock.day(), weekday),
                Point::new(left_cx, y),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
            y += 16;
            // 农历
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
            y += 16;
            // 星座 节气：节气当日显示“今日XX”，否则显示“XX第N天”
            let zodiac = get_zodiac(clock.month() as u8, clock.day());
            let term = get_solar_term(clock.year(), clock.month() as u8, clock.day());
            let term_day = solar_term_day(clock.year(), clock.month() as u8, clock.day());
            let mut term_str: heapless::String<16> = heapless::String::new();
            if term_day == 1 {
                let _ = write!(term_str, "今日{}", term);
            } else {
                let _ = write!(term_str, "{}第{}天", term, term_day);
            }
            let _ = font_small.render_aligned(
                format_args!("{} {}", zodiac, term_str),
                Point::new(left_cx, y),
                VerticalPosition::Top,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
    }

    // 右面板：白天图标 → 折线图 → 夜间图标
    let right_x = left_w + 1;
    let right_w = w - left_w - 1;
    let day_row_h: i32 = 44;
    let night_row_h: i32 = 56;

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

fn draw_clock<D>(display: &mut D, time: &str, center_x: i32, y: i32) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let character_style = SevenSegmentStyleBuilder::new()
        .digit_size(Size::new(22, 52))
        .segment_width(4)
        .segment_color(Black)
        .build();

    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Left)
        .baseline(Baseline::Top)
        .build();

    let text = Text::with_text_style(time, Point::new(0, y), character_style, text_style);
    let width = text.bounding_box().size.width as i32;
    text.translate(Point::new(center_x - width / 2, 0))
        .draw(display)?;

    Ok(())
}

pub fn sleep_renderer(display: &mut EpdDisplay) {
    let w = display.bounding_box().size.width as i32;
    let bat_y: i32 = 10;
    let cy = bat_y + 7;
    if crate::wifi::is_request_loading() {
        draw_loading_icon(Point::new(w - 118, cy), display);
    }
    draw_wifi_status(Point::new(w - 100, cy), display);
    draw_moon_icon(Point::new(w - 82, bat_y), display);
}
