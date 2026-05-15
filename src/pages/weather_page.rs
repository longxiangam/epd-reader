use alloc::boxed::Box;
use alloc::string::ToString;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, Primitive};
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyleBuilder};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::Black;
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use time::{OffsetDateTime, Weekday};
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, QUICKLY_LUT_CHANNEL, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::sleep::{refresh_active_time, to_sleep_tips};
use crate::storage::NvsStorage;
use crate::weather::{sync_holiday_success, sync_weather_success, HolidayInfo, Weather};
use crate::widgets::temp_chart::{TempPoint, draw_temp_chart, draw_temp_labels};
use crate::battery::BATTERY;
use crate::widgets::battery::draw_battery;
use crate::model::lunar::Lunar;
use crate::widgets::weather_icon::{WeatherKind, draw_weather_icon};
use crate::wifi::WIFI_STATE;
use crate::worldtime::{get_clock, sync_time_success};

fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "周一",
        Weekday::Tuesday => "周二",
        Weekday::Wednesday => "周三",
        Weekday::Thursday => "周四",
        Weekday::Friday => "周五",
        Weekday::Saturday => "周六",
        Weekday::Sunday => "周日",
    }
}

/// 绘制月亮图标（16x16），用两个圆做月牙效果
fn draw_moon_icon<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::On)
        .build();
    let erase = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::Off)
        .build();

    let r: u32 = 7;
    let cx = position.x + r as i32;
    let cy = position.y + r as i32;

    let _ = Circle::new(Point::new(cx - r as i32, cy - r as i32), r * 2)
        .into_styled(fill)
        .draw(target);
    let _ = Circle::new(Point::new(cx - r as i32 + 5, cy - r as i32 - 2), r * 2)
        .into_styled(erase)
        .draw(target);
}

fn sleep_renderer(display: &mut crate::display::EpdDisplay) {
    let w = display.bounding_box().size.width as i32;
    let bat_y = if w < 350 { 2 } else { 10 };
    let cy = bat_y + 7;
    if crate::wifi::is_request_loading() {
        draw_loading_icon(Point::new(w - 118, cy), display);
    }
    draw_wifi_status(Point::new(w - 100, cy), display);
    draw_moon_icon(Point::new(w - 82, bat_y), display);
}

/// 绘制WiFi状态图标（14x12）
fn draw_wifi_status<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .build();
    let dot = PrimitiveStyleBuilder::new()
        .fill_color(BinaryColor::On)
        .build();

    let x = position.x;
    let y = position.y;

    let state = embassy_futures::block_on(WIFI_STATE.lock());
    let connected = matches!(state.as_ref(), Some(crate::wifi::WifiNetState::WifiConnected));

    // 三条弧线 + 底部圆点
    Line::new(Point::new(x - 3, y), Point::new(x + 3, y))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 6, y - 3), Point::new(x + 6, y - 3))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 9, y - 6), Point::new(x + 9, y - 6))
        .into_styled(stroke.clone()).draw(target);
    Circle::new(Point::new(x - 1, y + 2), 3)
        .into_styled(dot.clone()).draw(target);

    if !connected {
        // 断开状态：画X
        Line::new(Point::new(x - 7, y - 7), Point::new(x + 7, y + 5))
            .into_styled(stroke).draw(target);
    }
}

/// 绘制Loading图标：上下箭头（12x14）
fn draw_loading_icon<D>(position: Point, target: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let stroke = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .build();

    let x = position.x;
    let y = position.y;

    // 上箭头
    Line::new(Point::new(x, y - 7), Point::new(x, y - 1))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 3, y - 4), Point::new(x, y - 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x + 3, y - 4), Point::new(x, y - 7))
        .into_styled(stroke.clone()).draw(target);
    // 下箭头
    Line::new(Point::new(x, y + 1), Point::new(x, y + 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x - 3, y + 4), Point::new(x, y + 7))
        .into_styled(stroke.clone()).draw(target);
    Line::new(Point::new(x + 3, y + 4), Point::new(x, y + 7))
        .into_styled(stroke).draw(target);
}

pub struct WeatherPage {
    running: bool,
    need_render: bool,
    current_date: Option<OffsetDateTime>,
}

impl WeatherPage {
    fn draw_clock<D>(display: &mut D, time: &str, position: Point) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let digit_w = 14;
        let digit_h = 30;
        let seg_w = 3;

        let character_style = SevenSegmentStyleBuilder::new()
            .digit_size(Size::new(digit_w, digit_h))
            .segment_width(seg_w)
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

    async fn back(&mut self) {
        self.running = false;
    }
}

impl Page for WeatherPage {
    fn new() -> Self {
        Self {
            running: false,
            need_render: false,
            current_date: None,
        }
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                let _ = display.clear_buffer(White);

                let w = display.bounding_box().size.width as i32;
                let h = display.bounding_box().size.height as i32;

                // 根据屏幕尺寸自适应字号和间距
                let is_small = w < 350;
                let font_small: FontRenderer = if is_small {
                    FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>()
                } else {
                    FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>()
                };
                let font_medium: FontRenderer = if is_small {
                    FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312b>()
                } else {
                    FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>()
                };

                if !sync_weather_success() {
                    let style = U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);
                    let mut wifi_finish = false;
                    if let Some(crate::wifi::WifiNetState::WifiConnecting) = *WIFI_STATE.lock().await {
                        let _ = Text::new("正在连接网络...", Point::new(0, 20), style.clone()).draw(display);
                    } else {
                        wifi_finish = true;
                    }
                    if wifi_finish {
                        let _ = Text::new("正在同步天气...", Point::new(0, 20), style.clone()).draw(display);
                    }
                    if !sync_holiday_success() {
                        let _ = Text::new("正在同步节假日...", Point::new(0, 40), style.clone()).draw(display);
                    }
                    RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                    return;
                }

                let weather = match Weather::get_weather().await {
                    Some(w) => w,
                    None => {
                        let style = U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);
                        let _ = Text::new("无天气数据", Point::new(0, 20), style).draw(display);
                        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                        return;
                    }
                };

                if weather.daily.is_empty() {
                    RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
                    return;
                }

                let daily = &weather.daily;

                // ═══ 布局区域划分（自上而下） ═══
                // 顶栏: 时钟 + 今日天气 + 日期 + 电量
                // 详情行(大屏): 湿度 + 风力 + 降水
                // 折线图区域
                // 底部: 天气图标 + 日期

                let header_h: i32 = if is_small { 34 } else { 36 };
                let left_w = w / 3;
                let chart_margin_y: i32 = if is_small { 6 } else { 12 };

                let separator_style = PrimitiveStyleBuilder::new()
                    .stroke_color(Black)
                    .stroke_width(1)
                    .build();

                // ── 顶栏 ──
                let today = &daily[0];

                // ── 顶栏：时钟 + 状态图标 + 电量 ──
                if let Some(clock) = self.current_date {
                    let time_str = format_args!("{:02}:{:02}", clock.hour(), clock.minute()).to_string();
                    let _ = Self::draw_clock(display, time_str.as_str(),
                        Point::new(if is_small { 0 } else { 4 }, if is_small { 2 } else { 3 }));
                }

                let bat_y = if is_small { 2 } else { 10 };
                let cy = bat_y + 7;
                // 网络请求Loading
                if crate::wifi::is_request_loading() {
                    draw_loading_icon(Point::new(w - 118, cy), display);
                }
                // WiFi状态
                draw_wifi_status(Point::new(w - 100, cy), display);
                // 电量
                if let Some(bat) = BATTERY.lock().await.as_ref() {
                    let _ = draw_battery(bat.percent, Point::new(w - 65, bat_y),
                        Black, &font_small, display);
                }

                // 分隔线1
                Line::new(Point::new(0, header_h), Point::new(w, header_h))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 中间区域：左详情 + 右折线图 ──
                // 左右分隔线（贯穿到底）
                Line::new(Point::new(left_w, header_h), Point::new(left_w, h))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 左面板：日期 + 天气详情 + 农历 ──
                if !is_small {
                    let left_cx = left_w / 2;
                    let mut y = header_h + 4;

                    // 日期 + 星期
                    if let Some(clock) = self.current_date {
                        let weekday = weekday_name(clock.weekday());
                        let _ = font_medium.render_aligned(
                            format_args!("{}.{} {}", clock.month() as u8, clock.day(), weekday),
                            Point::new(left_cx, y),
                            VerticalPosition::Top,
                            HorizontalAlignment::Center,
                            FontColor::Transparent(Black),
                            display,
                        );
                    }
                    y += 20;

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
                    y += 4;

                    // 农历日期
                    if let Some(clock) = self.current_date {
                        let lunar = Lunar::new(clock.year() as u16, clock.month() as u8);
                        if let Some(lunar_day) = lunar.get_lunar_day(clock.day()) {
                            let _ = font_medium.render_aligned(
                                format_args!("{}{}", lunar_day.get_month_name(), lunar_day.get_day_name()),
                                Point::new(left_cx, y),
                                VerticalPosition::Top,
                                HorizontalAlignment::Center,
                                FontColor::Transparent(Black),
                                display,
                            );
                        }
                    }
                } else {
                    let left_cx = left_w / 2;
                    let mut y = header_h + 2;

                    // 日期 + 星期
                    if let Some(clock) = self.current_date {
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

                    if let Some(clock) = self.current_date {
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

                // ── 右面板：白天图标 → 折线图 → 夜间图标 ──
                let right_x = left_w + 1;
                let right_w = w - left_w - 1;
                let day_row_h: i32 = if is_small { 36 } else { 44 };
                let night_row_h: i32 = if is_small { 36 } else { 56 };

                let day_count = daily.len() as i32;
                let col_w = right_w / day_count.max(1);

                // ── 右面板上部：白天图标 + 名称 ──
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

                // 白天与折线图之间的分隔线
                let sep_chart_top = day_icon_y + day_row_h;
                Line::new(Point::new(right_x, sep_chart_top), Point::new(w, sep_chart_top))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 右面板中间：温度折线图 ──
                let night_row_y = h - night_row_h;
                let chart_y = sep_chart_top + chart_margin_y;
                let chart_h = night_row_y - sep_chart_top - chart_margin_y * 2;

                // 折线图数据点与图标列对齐
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

                // 折线图与夜间之间的分隔线
                Line::new(Point::new(right_x, night_row_y), Point::new(w, night_row_y))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 右面板下部：夜间图标 + 名称 + 日期 ──
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

                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;
            }
        }
    }

    async fn run(&mut self, _spawner: Spawner) {
        self.running = true;
        crate::display::set_sleep_renderer(Some(sleep_renderer));
        refresh_active_time().await;
        let mut last_refresh_time = Instant::now();
        self.need_render = true;
        let mut wait_sync_time = true;
        let mut weather_last_update: Option<heapless::String<40>> = None;
        let mut holiday_sync_second = 0;

        loop {
            if !self.running {
                break;
            }

            if sync_time_success() {
                if Instant::now().duration_since(last_refresh_time).as_secs() > 60 || wait_sync_time {
                    if let Some(clock) = get_clock() {
                        self.current_date = Some(clock.local().await);
                    }
                    wait_sync_time = false;
                    self.need_render = true;
                    last_refresh_time = Instant::now();
                }
            } else {
                refresh_active_time().await;
                if Instant::now().duration_since(last_refresh_time).as_secs() > 5 {
                    self.need_render = true;
                    last_refresh_time = Instant::now();
                }
            }

            if sync_weather_success() {
                if let Some(weather) = Weather::get_weather().await {
                    match weather_last_update {
                        Some(ref v) => {
                            if !v.eq(&weather.last_update) {
                                self.need_render = true;
                                weather_last_update = Some(weather.last_update.clone());
                            }
                        }
                        None => {
                            self.need_render = true;
                            weather_last_update = Some(weather.last_update.clone());
                        }
                    }
                }
            } else {
                refresh_active_time().await;
                self.need_render = true;
                Timer::after(Duration::from_secs(1)).await;
            }

            if sync_holiday_success() {
                let temp = unsafe { crate::weather::HOLIDAY_SYNC_SECOND };
                if temp != holiday_sync_second {
                    self.need_render = true;
                    holiday_sync_second = temp;
                }
            } else {
                refresh_active_time().await;
                self.need_render = true;
                Timer::after(Duration::from_secs(1)).await;
            }

            Timer::after(Duration::from_millis(1)).await;
            self.render().await;
            if sync_time_success() && sync_weather_success() {
                let sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
                let weather_sleep_seconds = if sleep_storage.weather_sleep_seconds > 0 {
                    sleep_storage.weather_sleep_seconds
                } else {
                    5
                };
                to_sleep_tips(Duration::from_secs(60), Duration::from_secs(weather_sleep_seconds), true).await;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        crate::display::set_sleep_renderer(None);
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        // 短按1刷新天气
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                crate::wifi::set_request_loading(true);
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                mut_ref.render().await;
                QUICKLY_LUT_CHANNEL.send(true).await;
                let _ = Weather::request().await;
                crate::wifi::set_request_loading(false);
                QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                QUICKLY_LUT_CHANNEL.send(true).await;
            });
        }).await;
        // 短按2刷新节假日
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                crate::wifi::set_request_loading(true);
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                mut_ref.render().await;
                QUICKLY_LUT_CHANNEL.send(true).await;
                let _ = get_clock().unwrap().local().await;
                let _ = HolidayInfo::request().await;
                crate::wifi::set_request_loading(false);
                QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                QUICKLY_LUT_CHANNEL.send(true).await;
            });
        }).await;
        // 短按3退出
        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.running = false;
            });
        }).await;
    }
}
