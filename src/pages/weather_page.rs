use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use core::future::Future;
use eg_seven_segment::SevenSegmentStyleBuilder;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Point, Size};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Dimensions, DrawTarget, OriginDimensions, Primitive};
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyleBuilder, Rectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use esp_println::println;
use time::{OffsetDateTime, Weekday};
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use crate::display::{display_mut, QUICKLY_LUT_CHANNEL, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::model::seniverse::Daily;
use crate::pages::Page;
use crate::sleep::{refresh_active_time, to_sleep_tips};
use crate::storage::NvsStorage;
use crate::weather::{sync_holiday_success, sync_weather_success, HolidayInfo, Weather};
use crate::widgets::temp_chart::{TempPoint, draw_temp_chart, draw_temp_labels};
use crate::battery::BATTERY;
use crate::widgets::battery::draw_battery;
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
    use crate::storage::WeatherStorage;

    display.clear_buffer(White);

    let w = display.bounding_box().size.width as i32;
    let h = display.bounding_box().size.height as i32;
    let is_small = w < 350;

    let font_small: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312b>();
    let font_medium: FontRenderer = if is_small {
        FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312b>()
    } else {
        FontRenderer::new::<fonts::u8g2_font_wqy16_t_gb2312>()
    };

    let weather_storage = match WeatherStorage::read() {
        Ok(s) => s,
        Err(_) => return,
    };
    let weather = match weather_storage.weather_data {
        Some(ref d) => d,
        None => return,
    };
    if weather.daily.is_empty() {
        return;
    }
    let daily = &weather.daily;
    let today = &daily[0];

    let header_h: i32 = if is_small { 34 } else { 36 };
    let detail_h: i32 = if is_small { 0 } else { 18 };
    let bottom_h: i32 = if is_small { 36 } else { 42 };

    let separator_style = PrimitiveStyleBuilder::new()
        .stroke_color(Black)
        .stroke_width(1)
        .build();

    // 顶栏（无时钟，显示天气概要）
    let header_y: i32 = if is_small { 6 } else { 8 };
    let _ = font_medium.render_aligned(
        format_args!("{} {}/{}℃", today.text_day, today.high, today.low),
        Point::new(4, header_y),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        display,
    );

    // 月亮图标 + 电量（右上角）
    let bat_x = w - 65;
    let bat_y = if is_small { 2 } else { 10 };
    draw_moon_icon(Point::new(bat_x - 20, bat_y), display);
    let bat_percent = unsafe { crate::battery::LAST_BATTERY_PERCENT };
    let _ = draw_battery(bat_percent, Point::new(bat_x, bat_y), Black, &font_small, display);

    // 分隔线
    Line::new(Point::new(0, header_h), Point::new(w, header_h))
        .into_styled(separator_style.clone())
        .draw(display);

    // 详情行（大屏）
    if !is_small {
        let _ = font_medium.render_aligned(
            format_args!("湿度{}%  {}{}级  降水{}mm",
                today.humidity, today.wind_direction, today.wind_scale, today.rainfall),
            Point::new(4, header_h + 1),
            VerticalPosition::Top,
            HorizontalAlignment::Left,
            FontColor::Transparent(Black),
            display,
        );
        Line::new(Point::new(0, header_h + detail_h), Point::new(w, header_h + detail_h))
            .into_styled(separator_style.clone())
            .draw(display);
    }

    // 折线图
    let chart_top = header_h + detail_h;
    let chart_margin_y: i32 = if is_small { 10 } else { 12 };
    let chart_y = chart_top + chart_margin_y;
    let chart_h = h - bottom_h - chart_top - chart_margin_y * 2;
    let chart_x = 30;
    let chart_w = w - 40;

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

    // 底部
    let sep2_y = h - bottom_h;
    Line::new(Point::new(0, sep2_y), Point::new(w, sep2_y))
        .into_styled(separator_style)
        .draw(display);

    let icon_size = if is_small { 22 } else { 26 };
    let day_count = daily.len() as i32;
    let col_w = w / day_count.max(1);
    let icon_y = sep2_y + 4;

    for (i, day) in daily.iter().enumerate() {
        let cx = col_w * i as i32 + col_w / 2;
        let kind = WeatherKind::from_code(day.code_day.as_str());
        let _ = draw_weather_icon(kind, Point::new(cx, icon_y + icon_size as i32 / 2), icon_size, Black, display);
        let (_, date_part) = day.date.split_once('-').unwrap_or(("-", &day.date));
        let _ = font_small.render_aligned(
            format_args!("{}", date_part.replace("-", "/")),
            Point::new(cx, icon_y + icon_size as i32 + 3),
            VerticalPosition::Top,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
    }
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
                let detail_h: i32 = if is_small { 0 } else { 18 };
                let bottom_h: i32 = if is_small { 36 } else { 42 };
                let chart_margin_y: i32 = if is_small { 10 } else { 12 };

                let separator_style = PrimitiveStyleBuilder::new()
                    .stroke_color(Black)
                    .stroke_width(1)
                    .build();

                // ── 顶栏 ──
                let today = &daily[0];

                if let Some(clock) = self.current_date {
                    let time_str = format_args!("{:02}:{:02}", clock.hour(), clock.minute()).to_string();
                    let weekday = weekday_name(clock.weekday());

                    if is_small {
                        let _ = Self::draw_clock(display, time_str.as_str(), Point::new(0, 2));
                        // 时钟右侧：第一行 - 周几 + 天气 + 温度
                        let _ = font_small.render_aligned(
                            format_args!("{} {} {}/{}℃", weekday, today.text_day, today.high, today.low),
                            Point::new(68, 4),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            display,
                        );
                        // 第二行 - 湿度 + 风力
                        let _ = font_small.render_aligned(
                            format_args!("湿{}% {}{}级", today.humidity, today.wind_direction, today.wind_scale),
                            Point::new(68, 18),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            display,
                        );
                    } else {
                        let _ = Self::draw_clock(display, time_str.as_str(), Point::new(4, 3));
                        // 时钟右侧：天气 + 温度
                        let _ = font_medium.render_aligned(
                            format_args!("{} {}/{}℃", today.text_day, today.high, today.low),
                            Point::new(100, 9),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            display,
                        );
                        // 右侧：日期（紧凑格式）
                        let _ = font_medium.render_aligned(
                            format_args!("{}.{} {}", clock.month() as u8, clock.date(), weekday),
                            Point::new(w - 68, 9),
                            VerticalPosition::Top,
                            HorizontalAlignment::Right,
                            FontColor::Transparent(Black),
                            display,
                        );
                    }
                }

                // 电量图标（与日期同行对齐）
                if let Some(bat) = BATTERY.lock().await.as_ref() {
                    let bat_x = w - 65;
                    let bat_y = if is_small { 2 } else { 10 };
                    let _ = draw_battery(bat.percent, Point::new(bat_x, bat_y), Black, &font_small, display);
                }

                // 分隔线1
                let sep1_y = header_h;
                Line::new(Point::new(0, sep1_y), Point::new(w, sep1_y))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 详情行（仅大屏）──
                if !is_small {
                    let detail_y = sep1_y + 1;
                    let _ = font_medium.render_aligned(
                        format_args!("湿度{}%  {}{}级  降水{}mm",
                            today.humidity, today.wind_direction, today.wind_scale, today.rainfall),
                        Point::new(4, detail_y),
                        VerticalPosition::Top,
                        HorizontalAlignment::Left,
                        FontColor::Transparent(Black),
                        display,
                    );
                    // 详情行底部分隔
                    Line::new(Point::new(0, sep1_y + detail_h), Point::new(w, sep1_y + detail_h))
                        .into_styled(separator_style.clone())
                        .draw(display);
                }

                // ── 温度折线图 ──
                let chart_top = header_h + detail_h;
                let chart_y = chart_top + chart_margin_y;
                let chart_h = h - bottom_h - chart_top - chart_margin_y * 2;
                let chart_x = 30;
                let chart_w = w - 40;

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

                // 分隔线2
                let sep2_y = h - bottom_h;
                Line::new(Point::new(0, sep2_y), Point::new(w, sep2_y))
                    .into_styled(separator_style)
                    .draw(display);

                // ── 底部：天气图标 + 日期 ──
                let icon_size = if is_small { 22 } else { 26 };
                let day_count = daily.len() as i32;
                let col_w = w / day_count.max(1);
                let icon_y = sep2_y + 4;

                for (i, day) in daily.iter().enumerate() {
                    let cx = col_w * i as i32 + col_w / 2;

                    // 天气图标
                    let kind = WeatherKind::from_code(day.code_day.as_str());
                    let _ = draw_weather_icon(
                        kind,
                        Point::new(cx, icon_y + icon_size as i32 / 2),
                        icon_size,
                        Black,
                        display,
                    );

                    // 日期标签
                    let (_, date_part) = day.date.split_once('-').unwrap_or(("-", &day.date));
                    let date_display = date_part.replace("-", "/");
                    let _ = font_small.render_aligned(
                        format_args!("{}", date_display),
                        Point::new(cx, icon_y + icon_size as i32 + 3),
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

    async fn run(&mut self, spawner: Spawner) {
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
                let _ = Weather::request().await;
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                QUICKLY_LUT_CHANNEL.send(true).await;
            });
        }).await;
        // 短按2刷新节假日
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let _ = get_clock().unwrap().local().await;
                let _ = HolidayInfo::request().await;
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
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
