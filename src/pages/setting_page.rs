use alloc::{format, vec};
use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::MutexGuard;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Dimensions, OriginDimensions, Point, Size};
use embedded_graphics::prelude::{DrawTarget, DrawTargetExt, Primitive};
use embedded_graphics::primitives::{Line, PrimitiveStyleBuilder, Rectangle, StrokeAlignment};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_text::TextBox;
use esp_hal::system::software_reset;
use heapless::{String, Vec};
use u8g2_fonts::{FontRenderer, U8g2TextStyle};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use epd_waveshare::color::{Black, Color};
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::storage::{init_storage_area, NvsStorage, WIFI_INFO};
use crate::weather::Weather;
use crate::widgets::qrcode_widget::QrcodeWidget;
use crate::widgets::list_widget::ListWidget;
use crate::wifi::{finish_wifi, IP_ADDRESS, use_wifi, WIFI_MODEL, WifiNetError, WifiModel};
use crate::web_service::{web_service,STOP_WEB_SERVICE};

const SETTINGS_COUNT: usize = 3;
const READ_SLEEP_MIN: u64 = 10;
const WEATHER_SLEEP_MIN: u64 = 5;
const SLEEP_STEP: u64 = 5;

#[derive(Clone, Copy, PartialEq)]
enum SettingMode {
    QrCode,
    Settings { select: usize, editing: bool },
}

pub struct SettingPage {
    need_render:bool,
    running:bool,
    long_start_time:u64,
    reinit:bool,
    wifi_model: Option<WifiModel>,
    ip:String<20>,
    mode: SettingMode,
    read_sleep_seconds: u64,
    weather_sleep_seconds: u64,
}

impl SettingPage {
    fn settings_key_up(&mut self) {
        let (select, editing) = match self.mode {
            SettingMode::Settings { select, editing } => (select, editing),
            _ => return,
        };
        if editing {
            match select {
                0 => {
                    if self.read_sleep_seconds > READ_SLEEP_MIN {
                        self.read_sleep_seconds -= SLEEP_STEP;
                        if self.read_sleep_seconds < READ_SLEEP_MIN {
                            self.read_sleep_seconds = READ_SLEEP_MIN;
                        }
                    }
                }
                1 => {
                    if self.weather_sleep_seconds > WEATHER_SLEEP_MIN {
                        self.weather_sleep_seconds -= SLEEP_STEP;
                        if self.weather_sleep_seconds < WEATHER_SLEEP_MIN {
                            self.weather_sleep_seconds = WEATHER_SLEEP_MIN;
                        }
                    }
                }
                _ => {}
            }
        } else if select > 0 {
            self.mode = SettingMode::Settings { select: select - 1, editing: false };
        }
        self.need_render = true;
    }

    fn settings_key_down(&mut self) {
        let (select, editing) = match self.mode {
            SettingMode::Settings { select, editing } => (select, editing),
            _ => return,
        };
        if editing {
            match select {
                0 => { self.read_sleep_seconds += SLEEP_STEP; }
                1 => { self.weather_sleep_seconds += SLEEP_STEP; }
                _ => {}
            }
        } else if select < SETTINGS_COUNT - 1 {
            self.mode = SettingMode::Settings { select: select + 1, editing: false };
        }
        self.need_render = true;
    }

    fn settings_key_confirm(&mut self) {
        let (select, editing) = match self.mode {
            SettingMode::Settings { select, editing } => (select, editing),
            _ => return,
        };
        if editing {
            let mut sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
            sleep_storage.read_sleep_seconds = self.read_sleep_seconds;
            sleep_storage.weather_sleep_seconds = self.weather_sleep_seconds;
            let _ = sleep_storage.write();
            self.mode = SettingMode::Settings { select, editing: false };
        } else {
            match select {
                0 | 1 => { self.mode = SettingMode::Settings { select, editing: true }; }
                2 => { self.mode = SettingMode::QrCode; }
                _ => {}
            }
        }
        self.need_render = true;
    }

    fn render_settings(&self, display: &mut crate::display::EpdDisplay) {
        let _ = display.clear_buffer(White);
        let (select, editing) = match self.mode {
            SettingMode::Settings { select, editing } => (select, editing),
            _ => return,
        };

        let w = display.bounding_box().size.width;
        let h = display.bounding_box().size.height;

        let read_str = if editing && select == 0 {
            format!("阅读睡眠: >>{}s<<", self.read_sleep_seconds)
        } else {
            format!("阅读睡眠: {}s", self.read_sleep_seconds)
        };
        let weather_str = if editing && select == 1 {
            format!("天气睡眠: >>{}s<<", self.weather_sleep_seconds)
        } else {
            format!("天气睡眠: {}s", self.weather_sleep_seconds)
        };

        let mut items: Vec<&str, 20> = Vec::new();
        let _ = items.push(read_str.as_str());
        let _ = items.push(weather_str.as_str());
        let _ = items.push("返回");

        let mut list_widget = ListWidget::new(
            Point::new(0, 0), Black, epd_waveshare::color::White,
            Size::new(w, h), items,
        );
        list_widget.choose(select);
        let _ = list_widget.draw(display);
    }
}

impl Page for SettingPage {
    fn new() -> Self {
        let sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
        Self{
            need_render: false,
            running: false,
            long_start_time: 0,
            reinit:false,
            ip: Default::default(),
            wifi_model:None,
            mode: SettingMode::QrCode,
            read_sleep_seconds: if sleep_storage.read_sleep_seconds > 0 { sleep_storage.read_sleep_seconds } else { 120 },
            weather_sleep_seconds: if sleep_storage.weather_sleep_seconds > 0 { sleep_storage.weather_sleep_seconds } else { 60 },
        }
    }

    async fn render(&mut self) {
        if self.need_render {
            self.need_render = false;
            if let Some(display) = display_mut() {
                if matches!(self.mode, SettingMode::Settings { .. }) {
                    self.render_settings(display);
                    RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
                    return;
                }
                let _ = display.clear_buffer(White);
                
                
                
                let ip = unsafe { &*core::ptr::addr_of!(IP_ADDRESS) };
                let mut url:String<50> = String::new();
                url.push_str("http://");
                url.push_str(ip);
                url.push_str(":80");

                let qr_width =  display.bounding_box().size.width /2 ;
                let qrcode_widget = QrcodeWidget::new(&url,Point::new(0,0)
                                                      , Size::new(qr_width,qr_width )
                                                      , Black, epd_waveshare::color::White);
                qrcode_widget.draw(display);

                let style =
                    U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, Black);

                let right_x = qr_width as i32 + 8;
                let mut right_y = 10;

                if ip.is_empty() {
                    let _ = Text::new("正在连接网络", Point::new(right_x, right_y), style.clone())
                        .draw(display);
                } else {
                    let _ = Text::new(format!("地址：{}", url).as_str(), Point::new(right_x, right_y), style.clone())
                        .draw(display);
                }
                right_y += 20;

                match  self.wifi_model {
                    Some(WifiModel::AP) => {
                        let _ = Text::new("手机连接设备wifi", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                        right_y += 16;
                        let _ = Text::new("热点后配网", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                    }
                    Some(WifiModel::STA) => {
                        let _ = Text::new("扫码或输入地址配置", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                        right_y += 16;
                        let _ = Text::new("按中键进入睡眠设置", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                        right_y += 16;
                        let _ = Text::new("长按左键10秒重置", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                    }
                    None => {}
                }

                if self.long_start_time > 0 && !self.reinit   {
                    let secs =Instant::now().as_secs() - self.long_start_time;
                    let _ = Text::new( format!("已长按：{} 秒",secs).as_str(), Point::new(right_x, right_y), style.clone())
                        .draw(display);
                }
                if self.reinit {
                    let _ = Text::new("正在重置设备", Point::new(right_x, right_y), style.clone())
                        .draw(display);

                    RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
                    Timer::after(Duration::from_millis(500)).await;
                    init_storage_area();
                    software_reset();
                }


                RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep: false }).await;
            }
        }
    }

    async fn run(&mut self, spawner: Spawner) {
        crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
        STOP_WEB_SERVICE.reset();
        spawner.spawn(web_service().unwrap());
        let _ = spawner;
        self.running = true;
        self.need_render = true;
        match  *WIFI_MODEL.lock().await {
            Some(WifiModel::AP) => {
                self.wifi_model = Some(WifiModel::AP);
            }
            Some(WifiModel::STA) => {
                self.wifi_model = Some(WifiModel::STA);
            }
            None => {}
        }
        
        let mut has_ip = false;
        loop {
            if !self.running {
                break;
            }
            crate::wifi::refresh_last_time().await;
         
            if !has_ip && unsafe{!core::ptr::addr_of!(IP_ADDRESS).read().is_empty()}  {
                has_ip = true;
                self.need_render = true;
            }
            self.render().await;
            Timer::after(Duration::from_millis(1000)).await;
        }

        STOP_WEB_SERVICE.signal(());

        crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if matches!(mut_ref.mode, SettingMode::Settings { .. }) {
                    mut_ref.settings_key_down();
                } else {
                    crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                    mut_ref.need_render = true;
                    Timer::after(Duration::from_millis(50)).await;
                    crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                }
            });
        }).await;

        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if matches!(mut_ref.mode, SettingMode::Settings { .. }) {
                    mut_ref.settings_key_confirm();
                    if matches!(mut_ref.mode, SettingMode::QrCode) {
                        crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                    }
                } else {
                    mut_ref.running = false;
                }
            });
        }).await;

        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                match mut_ref.mode {
                    SettingMode::QrCode => {
                        mut_ref.mode = SettingMode::Settings { select: 0, editing: false };
                        crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                        mut_ref.need_render = true;
                    }
                    SettingMode::Settings { .. } => {
                        mut_ref.settings_key_up();
                    }
                }
            });
        }).await;


        event::on_target(EventType::KeyLongStart(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr.clone()).unwrap();
                mut_ref.long_start_time = Instant::now().as_secs();
            });
        }).await;

        event::on_target(EventType::KeyLongIng(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr.clone()).unwrap();
                if matches!(mut_ref.mode, SettingMode::Settings { editing: true, .. }) {
                    mut_ref.settings_key_up();
                    Timer::after(Duration::from_millis(200)).await;
                    return;
                }
                if (mut_ref.long_start_time == 0) {
                    mut_ref.long_start_time = Instant::now().as_secs();
                }
                mut_ref.need_render = true;
                if (Instant::now().as_secs() - mut_ref.long_start_time > 10) {
                    mut_ref.reinit = true;
                }
            });
        }).await;
        event::on_target(EventType::KeyLongEnd(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr.clone()).unwrap();
                mut_ref.long_start_time = 0;
            });
        }).await;

        event::on_target(EventType::KeyLongIng(2), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if matches!(mut_ref.mode, SettingMode::Settings { editing: true, .. }) {
                    mut_ref.settings_key_down();
                    Timer::after(Duration::from_millis(200)).await;
                }
            });
        }).await;
    }
}

