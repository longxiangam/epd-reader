use alloc::boxed::Box;
use alloc::string::ToString;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::prelude::Dimensions;
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::{Display, DisplayRotation};

use super::render_data::WeatherRenderData;
use crate::battery::BATTERY;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::sleep::{refresh_active_time, to_sleep_tips};
use crate::storage::NvsStorage;
use crate::weather::{sync_holiday_success, sync_weather_success, HolidayInfo, Weather};
use crate::wifi::WIFI_STATE;
use crate::worldtime::{clock_restored, get_clock, sync_time_success};

pub struct WeatherPage {
    pub(crate) running: bool,
    pub(crate) need_render: bool,
    pub(crate) current_date: Option<time::OffsetDateTime>,
}

impl WeatherPage {
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
                #[cfg(feature = "epd2in7")]
                display.set_rotation(DisplayRotation::Rotate90);

                let _ = display.clear_buffer(White);

                let (w, h) = if cfg!(feature = "epd2in7") {
                    (display.bounding_box().size.height as i32,
                     display.bounding_box().size.width as i32)
                } else {
                    (display.bounding_box().size.width as i32,
                     display.bounding_box().size.height as i32)
                };

                // 收集布局数据
                let battery_percent = BATTERY.lock().await.as_ref().map(|b| b.percent);
                let wifi_state = WIFI_STATE.lock().await;
                let wifi_connected = matches!(wifi_state.as_ref(), Some(crate::wifi::WifiNetState::WifiConnected));
                let wifi_connecting = matches!(*wifi_state, Some(crate::wifi::WifiNetState::WifiConnecting));
                drop(wifi_state);
                let request_loading = crate::wifi::is_request_loading();
                let weather_synced = sync_weather_success();
                let weather = if weather_synced {
                    Weather::get_weather().await
                } else {
                    None
                };
                let holiday_synced = sync_holiday_success();
                let time_synced = sync_time_success();

                let data = WeatherRenderData {
                    w,
                    h,
                    current_date: self.current_date,
                    battery_percent,
                    wifi_connected,
                    wifi_connecting,
                    request_loading,
                    weather: weather.as_ref(),
                    weather_synced,
                    holiday_synced,
                    time_synced,
                    weather_sync_second: unsafe { crate::weather::WEATHER_SYNC_SECOND },
                };

                let _ = super::draw(display, &data);

                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: true }).await;

                #[cfg(feature = "epd2in7")]
                display.set_rotation(DisplayRotation::Rotate0);
            }
        }
    }

    async fn run(&mut self, _spawner: Spawner) {
        self.running = true;
        crate::display::set_sleep_renderer(Some(super::sleep_renderer));
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

            if clock_restored() {
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
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                mut_ref.render().await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                let _ = Weather::request().await;
                crate::wifi::set_request_loading(false);
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                Timer::after(Duration::from_millis(50)).await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            });
        }).await;
        // 短按2刷新节假日
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                crate::wifi::set_request_loading(true);
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.need_render = true;
                mut_ref.render().await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                let _ = get_clock().unwrap().local().await;
                let _ = HolidayInfo::request().await;
                crate::wifi::set_request_loading(false);
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
