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
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use esp_println::println;
use time::OffsetDateTime;
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
use crate::widgets::weather_icon::{WeatherKind, draw_weather_icon};
use crate::wifi::WIFI_STATE;
use crate::worldtime::{get_clock, sync_time_success};

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
                // 顶栏: 时间 + 日期 + 今日概况     高度 ~50px
                // 分隔线
                // 折线图区域                       高度 ~120px (大屏) / ~70px (小屏)
                // 分隔线
                // 底部: 天气图标 + 日期标签        高度 ~70px (大屏) / ~50px (小屏)

                let header_h = if is_small { 38 } else { 50 };
                let bottom_h = if is_small { 42 } else { 70 };
                let chart_margin_y = 14; // 图表上下留白给温度标签

                let separator_style = PrimitiveStyleBuilder::new()
                    .stroke_color(Black)
                    .stroke_width(1)
                    .build();

                // ── 顶栏 ──
                let today = &daily[0];
                let mut header_y = 4;

                // 左侧：时钟
                if let Some(clock) = self.current_date {
                    let time_str = format_args!("{:02}:{:02}", clock.hour(), clock.minute()).to_string();
                    if is_small {
                        let _ = Self::draw_clock(display, time_str.as_str(), Point::new(0, header_y));
                        header_y += 32;
                    } else {
                        let _ = Self::draw_clock(display, time_str.as_str(), Point::new(4, header_y + 2));
                        // 右侧日期
                        let date_str = format_args!(
                            "{}.{}.{:02}",
                            clock.year(), clock.month() as u8, clock.date(),
                        ).to_string();
                        let _ = font_medium.render_aligned(
                            format_args!("{}", date_str),
                            Point::new(w - 8, header_y + 8),
                            VerticalPosition::Top,
                            HorizontalAlignment::Right,
                            FontColor::Transparent(Black),
                            display,
                        );
                        // 今日天气描述（时钟右侧）
                        let _ = font_medium.render_aligned(
                            format_args!("{} {}", today.text_day, today.high),
                            Point::new(100, header_y + 8),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            display,
                        );
                    }
                }

                if is_small {
                    // 小屏：日期和今日天气在时钟下方一行
                    if let Some(clock) = self.current_date {
                        let date_str = format_args!(
                            "{}.{:02}.{} {} {}",
                            clock.year(), clock.month() as u8, clock.date(),
                            today.text_day, today.high,
                        ).to_string();
                        let _ = font_small.render_aligned(
                            format_args!("{}", date_str),
                            Point::new(68, 8),
                            VerticalPosition::Top,
                            HorizontalAlignment::Left,
                            FontColor::Transparent(Black),
                            display,
                        );
                    }
                }

                // 分隔线1
                let sep1_y = header_h;
                Line::new(Point::new(0, sep1_y), Point::new(w, sep1_y))
                    .into_styled(separator_style.clone())
                    .draw(display);

                // ── 温度折线图 ──
                let chart_y = sep1_y + chart_margin_y;
                let chart_h = h - bottom_h - sep1_y - chart_margin_y * 2;
                let chart_x = 30;
                let chart_w = w - 40;

                let temp_points: heapless::Vec<TempPoint, 10> = daily.iter().map(|d| {
                    let hi: i32 = d.high.as_str().parse().unwrap_or(0);
                    let lo: i32 = d.low.as_str().parse().unwrap_or(0);
                    TempPoint { label: "", high: hi, low: lo }
                }).collect();

                // 参考线
                let _ = draw_temp_chart(
                    Point::new(chart_x, chart_y),
                    Size::new(chart_w as u32, chart_h as u32),
                    &temp_points,
                    Black,
                    display,
                );

                // 温度标签
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

                // ── 底部：天气图标 + 日期标签 ──
                let icon_size = if is_small { 22 } else { 32 };
                let day_count = daily.len() as i32;
                let col_w = w / day_count.max(1);
                let icon_y = sep2_y + 6;

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
                        Point::new(cx, icon_y + icon_size as i32 + 4),
                        VerticalPosition::Top,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(Black),
                        display,
                    );

                    // 天气描述
                    let _ = font_small.render_aligned(
                        format_args!("{}", day.text_day),
                        Point::new(cx, icon_y + icon_size as i32 + 16),
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
