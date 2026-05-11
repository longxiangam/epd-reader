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
use embedded_graphics::prelude::{Dimensions, DrawTarget, OriginDimensions};
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use esp_println::println;
use time::OffsetDateTime;
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use crate::display::{display_mut, QUICKLY_LUT_CHANNEL, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::battery::BATTERY;
use crate::widgets::battery::draw_battery;
use crate::sleep::{refresh_active_time, to_sleep, to_sleep_tips};
use crate::storage::NvsStorage;
use crate::weather::{sync_holiday_success, sync_weather_success, HolidayInfo, Weather};
use crate::widgets::calendar::Calendar;
use crate::wifi::WIFI_STATE;
use crate::worldtime::{get_clock, sync_time_success};

pub struct CalendarPage {
    running: bool,
    need_render: bool,
    current_date: Option<OffsetDateTime>,
}

impl CalendarPage {
    fn draw_clock<D>(display: &mut D, time: &str) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let character_style = SevenSegmentStyleBuilder::new()
            .digit_size(Size::new(18, 43))
            .segment_width(4)
            .segment_color(Black)
            .build();

        let text_style = TextStyleBuilder::new()
            .alignment(Alignment::Left)
            .baseline(Baseline::Top)
            .build();

        Text::with_text_style(
            time,
            Point::new(0, (display.bounding_box().size.height - 45) as i32),
            character_style,
            text_style,
        )
            .draw(display)?;

        Ok(())
    }

    async fn back(&mut self) {
        self.running = false;
    }
}

impl Page for CalendarPage {
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

                // 电量图标（右上角）
                {
                    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy12_t_gb2312>();
                    if let Some(bat) = BATTERY.lock().await.as_ref() {
                        let bat_x = display.bounding_box().size.width as i32 - 60;
                        let _ = draw_battery(bat.percent, Point::new(bat_x, 4), Black, &font, display);
                    }
                }

                let style =
                    U8g2TextStyle::new(fonts::u8g2_font_wqy16_t_gb2312, Black);

                // 天气数据（右侧）
                if sync_weather_success() {
                    if let Some(weather) = Weather::get_weather().await {
                        let x = 100;
                        let mut y = display.size().height - 45;
                        for one in weather.daily.iter() {
                            let (_, date) = one.date.split_once('-').unwrap();
                            let date = date.replace("-", ".");
                            let str = format_args!(
                                "{} {}/{},{}/{}℃,湿:{}%,风:{}",
                                date, one.text_day, one.text_night, one.low, one.high, one.humidity, one.wind_scale
                            ).to_string();
                            let _ = Text::new(str.as_str(), Point::new(x, y as i32), style.clone()).draw(display);
                            y += 20;
                        }
                    }
                } else {
                    let mut wifi_finish = false;
                    if let Some(crate::wifi::WifiNetState::WifiConnecting) = *WIFI_STATE.lock().await {
                        let _ = Text::new("正在连接网络...", Point::new(0, 20), style.clone()).draw(display);
                    } else {
                        wifi_finish = true;
                    }
                    if wifi_finish {
                        let _ = Text::new("正在同步天气...", Point::new(0, 20), style.clone()).draw(display);
                    }
                }

                // 节假日同步状态
                if !sync_holiday_success() {
                    let mut wifi_finish = false;
                    if let Some(crate::wifi::WifiNetState::WifiConnecting) = *WIFI_STATE.lock().await {
                        let _ = Text::new("正在连接网络...", Point::new(0, 40), style.clone()).draw(display);
                    } else {
                        wifi_finish = true;
                    }
                    if wifi_finish {
                        let _ = Text::new("正在同步节假日...", Point::new(0, 40), style.clone()).draw(display);
                    }
                }

                // 日历 + 时钟
                if sync_time_success() {
                    if let Some(clock) = self.current_date {
                        let local = clock;
                        let hour = local.hour();
                        let minute = local.minute();

                        let str = format_args!("{:02}:{:02}", hour, minute).to_string();
                        Self::draw_clock(display, str.as_str());

                        let height = display.size().height;
                        let width = display.size().width;

                        let calendar_rect = Rectangle::new(
                            Point::new(0, 0),
                            Size::new(width, height - 45),
                        );

                        let year = local.year();
                        let month = local.month();
                        let today = local.date();
                        let mut calendar = Calendar::new(
                            Point::default(), Size::default(),
                            year, month, today, Black, epd_waveshare::color::White,
                        );
                        calendar.position = calendar_rect.top_left;
                        calendar.size = calendar_rect.size;
                        let _ = calendar.draw(display);
                    }
                } else {
                    let _ = Text::new("正在同步时间...", Point::new(0, 200), style.clone()).draw(display);
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
                let calendar_sleep_seconds = if sleep_storage.weather_sleep_seconds > 0 {
                    sleep_storage.weather_sleep_seconds
                } else {
                    5
                };
                to_sleep_tips(Duration::from_secs(60), Duration::from_secs(calendar_sleep_seconds), true).await;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        // 上一月
        event::on_target(EventType::KeyLongEnd(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if let Some(ref mut clock) = mut_ref.current_date {
                    let year = clock.year();
                    let pre_month = clock.month().previous();
                    if let Some(current_clock) = get_clock() {
                        let now = current_clock.local().await;
                        if now.month() == pre_month && now.year() == year {
                            *clock = now;
                        } else if clock.month() as u8 > 1 {
                            *clock = clock.replace_day(1).unwrap();
                            *clock = clock.replace_month(pre_month).unwrap();
                        } else {
                            *clock = clock.replace_year(year - 1).unwrap();
                            *clock = clock.replace_day(1).unwrap();
                            *clock = clock.replace_month(pre_month).unwrap();
                        }
                    }
                    mut_ref.need_render = true;
                }
            });
        }).await;
        // 下一月
        event::on_target(EventType::KeyLongEnd(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if let Some(ref mut clock) = mut_ref.current_date {
                    let year = clock.year();
                    let next_month = clock.month().next();
                    if let Some(current_clock) = get_clock() {
                        let now = current_clock.local().await;
                        if now.month() == next_month && now.year() == year {
                            *clock = now;
                        } else if 12 > clock.month() as u8 {
                            *clock = clock.replace_day(1).unwrap();
                            *clock = clock.replace_month(next_month).unwrap();
                        } else {
                            *clock = clock.replace_year(year + 1).unwrap();
                            *clock = clock.replace_day(1).unwrap();
                            *clock = clock.replace_month(next_month).unwrap();
                        }
                    }
                    mut_ref.need_render = true;
                }
            });
        }).await;
        // 长按3回到当前月
        event::on_target(EventType::KeyLongEnd(3), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if let Some(clock) = get_clock() {
                    mut_ref.current_date = Some(clock.local().await);
                    mut_ref.need_render = true;
                }
            });
        }).await;
        // 短按1刷新天气
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let _ = Weather::request().await;
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            });
        }).await;
        // 短按2刷新节假日
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let local = get_clock().unwrap().local().await;
                let _ = HolidayInfo::request().await;
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
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
