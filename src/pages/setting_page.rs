use alloc::format;
use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::geometry::{Dimensions, Point, Size};
use embedded_graphics::text::Text;
use esp_hal::system::software_reset;
use heapless::{String, Vec};
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts;
use epd_waveshare::color::Black;
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::Display;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::pages::Page;
use crate::storage::{init_storage_area, NvsStorage};
use crate::widgets::qrcode_widget::QrcodeWidget;
use crate::widgets::list_widget::ListWidget;
use crate::wifi::{IP_ADDRESS, WIFI_MODEL, WifiModel};
use crate::web_service::{web_service,STOP_WEB_SERVICE};

const SETTINGS_COUNT: usize = 5;
const READ_SLEEP_MIN: u64 = 10;
const WEATHER_SLEEP_MIN: u64 = 5;
const SLEEP_STEP: u64 = 5;

#[derive(Clone, Copy, PartialEq)]
enum SettingMode {
    QrCode,
    Settings { select: usize, editing: bool },
    Locate,
}

enum LocateState {
    Idle,
    Requesting,
    Success { city: String<32>, latlon: String<32> },
    Failed,
}

pub struct SettingPage {
    need_render:bool,
    running:bool,
    long_start_time:u64,
    reinit:bool,
    wifi_model: Option<WifiModel>,
    ip:String<20>,
    mode: SettingMode,
    locate_state: LocateState,
    web_service_running: bool,
    last_location: Option<(String<32>, String<32>)>,
    last_location_loaded: bool,
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
                2 => {
                    // 自动定位：先进入界面，再由按键触发定位
                    self.mode = SettingMode::Locate;
                    self.locate_state = LocateState::Idle;
                }
                3 => { self.mode = SettingMode::QrCode; }
                4 => {
                    // 返回：STA 退出设置页回到主菜单；AP（首次配网）返回二维码根界面
                    match self.wifi_model {
                        Some(WifiModel::STA) => { self.running = false; }
                        _ => { self.mode = SettingMode::QrCode; }
                    }
                }
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
        let _ = items.push("自动定位");
        let _ = items.push("web配置");
        let _ = items.push("返回");

        let mut list_widget = ListWidget::new(
            Point::new(0, 0), Black, epd_waveshare::color::White,
            Size::new(w, h), items,
        );
        list_widget.choose(select);
        let _ = list_widget.draw(display);
    }

    fn render_locate(&self, display: &mut crate::display::EpdDisplay) {
        let _ = display.clear_buffer(White);
        let style = U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, Black);
        match &self.locate_state {
            LocateState::Idle => {
                let _ = Text::new("自动定位", Point::new(10, 20), style.clone())
                    .draw(display);
                let mut y = 48;
                let mut shown = false;
                if let Some((city, latlon)) = &self.last_location {
                    if !city.is_empty() {
                        let _ = Text::new(format!("城市:{}", city).as_str(), Point::new(10, y), style.clone())
                            .draw(display);
                        y += 20;
                        shown = true;
                    }
                    if latlon.contains(':') {
                        let _ = Text::new(format!("坐标:{}", latlon).as_str(), Point::new(10, y), style.clone())
                            .draw(display);
                        y += 20;
                        shown = true;
                    }
                }
                if !shown {
                    let _ = Text::new("未定位", Point::new(10, y), style.clone())
                        .draw(display);
                    y += 20;
                }
                let _ = Text::new("按左键开始定位", Point::new(10, 100), style.clone())
                    .draw(display);
                let _ = Text::new("右键返回", Point::new(10, 120), style.clone())
                    .draw(display);
            }
            LocateState::Requesting => {
                let _ = Text::new("正在定位...", Point::new(10, 20), style.clone())
                    .draw(display);
            }
            LocateState::Success { city, latlon } => {
                let _ = Text::new("定位成功", Point::new(10, 20), style.clone())
                    .draw(display);
                let _ = Text::new(format!("城市:{}", city).as_str(), Point::new(10, 44), style.clone())
                    .draw(display);
                let _ = Text::new(format!("经纬度:{}", latlon).as_str(), Point::new(10, 64), style.clone())
                    .draw(display);
                let _ = Text::new("左键重新定位 右键返回", Point::new(10, 92), style.clone())
                    .draw(display);
            }
            LocateState::Failed => {
                let _ = Text::new("定位失败", Point::new(10, 20), style.clone())
                    .draw(display);
                let _ = Text::new("请检查网络", Point::new(10, 44), style.clone())
                    .draw(display);
                let _ = Text::new("左键重试 右键返回", Point::new(10, 72), style.clone())
                    .draw(display);
            }
        }
    }

    /// 执行一次定位并立即拉取天气，更新定位状态
    async fn do_locate(&mut self) {
        match crate::location::locate().await {
            Some(r) => {
                let city = r.city.clone();
                let latlon = r.latlon.clone();
                // 将 "lat:lon" 写入天气配置的城市字段（心知天气 location 参数可直接使用）
                let mut ws = crate::storage::WeatherStorage::read().unwrap_or_default();
                ws.city = latlon.clone();
                let _ = ws.write();
                // 立即用新坐标拉取一次天气
                let _ = crate::weather::Weather::request().await;
                self.locate_state = LocateState::Success { city, latlon };
            }
            None => {
                self.locate_state = LocateState::Failed;
            }
        }
        self.need_render = true;
    }

    /// 从存储读取已保存的定位信息（城市名 + 坐标），用于定位界面展示
    fn load_last_location(&mut self) {
        self.last_location = None;
        if let Ok(ws) = crate::storage::WeatherStorage::read() {
            if !ws.city.is_empty() {
                let city: String<32> = ws
                    .weather_data
                    .as_ref()
                    .map(|d| d.location.name.chars().collect())
                    .unwrap_or_default();
                self.last_location = Some((city, ws.city.clone()));
            }
        }
    }

    fn exit_locate(&mut self) {
        // 返回列表，停留在「自动定位」项
        self.mode = SettingMode::Settings { select: 2, editing: false };
        self.locate_state = LocateState::Idle;
        self.last_location_loaded = false;
        self.need_render = true;
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
            locate_state: LocateState::Idle,
            web_service_running: false,
            last_location: None,
            last_location_loaded: false,
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
                if matches!(self.mode, SettingMode::Locate) {
                    self.render_locate(display);
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
                        let _ = Text::new("按中键返回菜单", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                        right_y += 16;
                        let _ = Text::new("长按左键10秒重置", Point::new(right_x, right_y), style.clone())
                            .draw(display);
                    }
                    None => {}
                }
                right_y += 16; // 倒计时/重置提示下移一行，避免与上方提示文字重叠

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
        self.running = true;
        self.need_render = true;
        match  *WIFI_MODEL.lock().await {
            Some(WifiModel::AP) => {
                // 首次配网：默认显示二维码与地址，保持原有行为不受影响
                self.wifi_model = Some(WifiModel::AP);
                self.mode = SettingMode::QrCode;
            }
            Some(WifiModel::STA) => {
                // 正常进入设置：默认显示列表菜单
                self.wifi_model = Some(WifiModel::STA);
                self.mode = SettingMode::Settings { select: 0, editing: false };
            }
            None => {
                self.mode = SettingMode::Settings { select: 0, editing: false };
            }
        }

        let mut has_ip = false;
        let mut first_render = true;
        loop {
            if !self.running {
                break;
            }
            crate::sleep::refresh_active_time().await;

            // web_service 只在二维码(web配置)界面运行
            let need_web = matches!(self.mode, SettingMode::QrCode);
            if need_web && !self.web_service_running {
                STOP_WEB_SERVICE.reset();
                let _ = spawner.spawn(web_service().unwrap());
                self.web_service_running = true;
            } else if !need_web && self.web_service_running {
                STOP_WEB_SERVICE.signal(());
                self.web_service_running = false;
            }

            // 进入定位界面时，加载已保存的定位信息用于展示
            if matches!(self.mode, SettingMode::Locate) && !self.last_location_loaded {
                self.load_last_location();
                self.last_location_loaded = true;
            }

            // 自动定位：请求中时执行一次网络定位 + 拉取天气
            if matches!(self.mode, SettingMode::Locate)
                && matches!(self.locate_state, LocateState::Requesting)
            {
                self.do_locate().await;
            }

            if !has_ip && unsafe{!core::ptr::addr_of!(IP_ADDRESS).read().is_empty()}  {
                has_ip = true;
                // 只有二维码界面需要根据 IP 变化重绘（显示地址），列表无需刷新
                if matches!(self.mode, SettingMode::QrCode) {
                    self.need_render = true;
                }
            }
            self.render().await;
            // 首屏全刷清晰显示后，列表导航切换为快刷；二维码界面保持全刷
            if first_render {
                first_render = false;
                if matches!(self.mode, SettingMode::Settings { .. }) {
                    crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                }
            }
            Timer::after(Duration::from_millis(1000)).await;
        }

        if self.web_service_running {
            STOP_WEB_SERVICE.signal(());
            self.web_service_running = false;
        }

        crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if matches!(mut_ref.mode, SettingMode::Settings { .. }) {
                    mut_ref.settings_key_down();
                } else if matches!(mut_ref.mode, SettingMode::QrCode) {
                    // web配置：左键全刷二维码
                    crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                    mut_ref.need_render = true;
                    Timer::after(Duration::from_millis(50)).await;
                    crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                } else if matches!(mut_ref.mode, SettingMode::Locate) {
                    // 自动定位：左键触发定位（定位进行中除外）
                    if !matches!(mut_ref.locate_state, LocateState::Requesting) {
                        mut_ref.locate_state = LocateState::Requesting;
                        mut_ref.need_render = true;
                    }
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
                } else if matches!(mut_ref.mode, SettingMode::Locate) {
                    // 自动定位：右键返回菜单列表（定位进行中除外）
                    if !matches!(mut_ref.locate_state, LocateState::Requesting) {
                        mut_ref.exit_locate();
                    }
                } else if matches!(mut_ref.mode, SettingMode::QrCode) {
                    // web配置：右键返回菜单列表
                    mut_ref.mode = SettingMode::Settings { select: 3, editing: false };
                    crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
                    mut_ref.need_render = true;
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
                    SettingMode::Locate => {
                        if !matches!(mut_ref.locate_state, LocateState::Requesting) {
                            mut_ref.exit_locate();
                        }
                    }
                }
            });
        }).await;


        event::on_target(EventType::KeyLongStart(1), Self::mut_to_ptr(self), move |info| {
            return Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr.clone()).unwrap();
                // 重置只在二维码界面生效，列表界面长按左键无效
                if matches!(mut_ref.mode, SettingMode::QrCode) {
                    mut_ref.long_start_time = Instant::now().as_secs();
                }
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
                // 重置只在二维码界面生效，列表界面长按左键无效
                if !matches!(mut_ref.mode, SettingMode::QrCode) {
                    return;
                }
                if mut_ref.long_start_time == 0  {
                    mut_ref.long_start_time = Instant::now().as_secs();
                }
                mut_ref.need_render = true;
                if Instant::now().as_secs() - mut_ref.long_start_time > 10  {
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

