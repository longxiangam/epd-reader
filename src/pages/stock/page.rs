use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_println::println;
use embedded_graphics::prelude::Dimensions;
use epd_waveshare::color::Color::White;
use epd_waveshare::prelude::{Display, DisplayRotation};

use super::render_data::StockRenderData;
use crate::display::{display_mut, RENDER_CHANNEL, RenderInfo};
use crate::event;
use crate::event::EventType;
use crate::model::stock::{
    self, parse_kline, parse_quote, ChartMode, StockData, DEFAULT_STOCK,
};
use crate::pages::Page;
use crate::request::{RequestClient, RequestError};
use crate::sleep::{refresh_active_time, to_sleep_tips};
use crate::storage::NvsStorage;
use esp_hal::ram;

/// 股票当前图模式，存 rtc_fast，跨深睡重启保留。
/// 深睡唤醒会重启程序，若不保留则模式被重置为默认 Day，分时的 2 分钟周期就失效了。
#[ram(unstable(rtc_fast))]
static mut STOCK_MODE: u8 = 4; // 默认分时（Minute=4）

pub struct StockPage {
    pub(crate) running: bool,
    pub(crate) need_render: bool,
    pub(crate) mode: ChartMode,
    pub(crate) data: Option<Box<StockData>>,
    pub(crate) loading: bool,
    pub(crate) err_msg: Option<&'static str>,
}

impl StockPage {
    async fn back(&mut self) {
        self.running = false;
    }

    /// 切换 分时/日K/周K/月K/折线（forward=true 下一个，false 上一个）。
    /// 若新旧模式同数据源（日K↔折线），只换渲染方式不重新请求；否则清空数据触发重新请求。
    fn switch_mode(&mut self, forward: bool) {
        let old_source = self.mode.source();
        let new_mode = if forward { self.mode.next() } else { self.mode.prev() };
        let new_source = new_mode.source();
        if new_source != old_source {
            self.data = None;
        }
        self.mode = new_mode;
        unsafe { *core::ptr::addr_of_mut!(STOCK_MODE) = new_mode.encode(); }
    }

    /// 切换查询的股票（长按1/2）。更新 StockStorage.selected 并触发重新拉取。
    fn switch_stock(&mut self, forward: bool) {
        let mut ss = crate::storage::StockStorage::read().unwrap_or_default();
        if ss.count > 1 {
            let c = ss.count as usize;
            let cur = (ss.selected as usize).min(c - 1);
            let nxt = if forward { (cur + 1) % c } else { (cur + c - 1) % c };
            ss.selected = nxt as u8;
            let _ = ss.write();
            self.data = None;
        }
    }

    async fn fetch(&mut self) {
        // 先设加载标志，确保渲染时右上角加载图标可见
        crate::wifi::set_request_loading(true);
        self.loading = true;
        self.need_render = true;
        self.render().await;
        // 读 web 配置的股票（选中那支）；未配置则用默认 sh600519
        let code_storage = crate::storage::StockStorage::read().unwrap_or_default();
        let (code, name) = if code_storage.count > 0 {
            let i = (code_storage.selected as usize).min((code_storage.count as usize).saturating_sub(1));
            (code_storage.entries[i].code.as_str(), code_storage.entries[i].name.as_str())
        } else {
            (DEFAULT_STOCK, "")
        };
        match fetch_stock(self.mode, code, name).await {
            Ok(d) => {
                self.data = Some(d);
                self.err_msg = None;
            }
            Err(msg) => {
                self.err_msg = Some(msg);
            }
        }
        self.loading = false;
        self.need_render = true;
    }
}

impl Page for StockPage {
    fn new() -> Self {
        Self {
            running: false,
            need_render: false,
            mode: ChartMode::Minute,
            data: None,
            loading: false,
            err_msg: None,
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

                let battery_percent = crate::battery::BATTERY.lock().await.as_ref().map(|b| b.percent);
                let wifi_state = crate::wifi::WIFI_STATE.lock().await;
                let wifi_connected = matches!(wifi_state.as_ref(), Some(crate::wifi::WifiNetState::WifiConnected));
                let wifi_connecting = matches!(*wifi_state, Some(crate::wifi::WifiNetState::WifiConnecting));
                drop(wifi_state);
                let request_loading = crate::wifi::is_request_loading();

                let data = StockRenderData {
                    w,
                    h,
                    mode: self.mode,
                    data: self.data.as_deref(),
                    loading: self.loading,
                    err_msg: self.err_msg,
                    battery_percent,
                    wifi_connected,
                    wifi_connecting,
                    request_loading,
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
        // 深睡唤醒会重启程序，从 rtc_fast 恢复上次图模式
        self.mode = ChartMode::decode(unsafe { *core::ptr::addr_of!(STOCK_MODE) });
        crate::display::set_sleep_renderer(Some(super::sleep_renderer));
        refresh_active_time().await;
        // 进入即拉取一次当前模式
        self.fetch().await;
        loop {
            if !self.running {
                break;
            }
            // 注意：此处不能再无条件 refresh_active_time()，否则空闲时间永远归零、永不睡眠。
            // 活动时间由按键（event::run 内部）刷新，空闲 30 秒后 to_sleep_tips 自动入睡。
            if self.need_render {
                self.render().await;
            }
            // 分时模式每 2 分钟唤醒拉取一次；其它模式每 12 小时拉取一次。
            // 深睡唤醒 = 重启，重启后重新进入页面时的初始 fetch() 即完成刷新；
            // 模式经 rtc_fast(STOCK_MODE) 跨重启保留，故下次入睡时长仍正确。
            let sleep_secs: u64 = if self.mode.is_realtime() { 120 } else { 12 * 3600 };
            to_sleep_tips(Duration::from_secs(sleep_secs), Duration::from_secs(30), true).await;
            Timer::after(Duration::from_millis(50)).await;
        }
        crate::display::set_sleep_renderer(None);
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        // 短按1：上一个图模式
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.switch_mode(false);
                if mut_ref.data.is_none() {
                    mut_ref.fetch().await;
                } else {
                    mut_ref.need_render = true;
                    mut_ref.render().await;
                }
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 短按2：下一个图模式
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.switch_mode(true);
                if mut_ref.data.is_none() {
                    mut_ref.fetch().await;
                } else {
                    mut_ref.need_render = true;
                    mut_ref.render().await;
                }
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 长按1：上一支股票
        event::on_target(EventType::KeyLongEnd(1), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.switch_stock(false);
                if mut_ref.data.is_none() {
                    mut_ref.fetch().await;
                }
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 长按2：下一支股票
        event::on_target(EventType::KeyLongEnd(2), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.switch_stock(true);
                if mut_ref.data.is_none() {
                    mut_ref.fetch().await;
                }
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 短按3：返回
        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.back().await;
            })
        }).await;
        // 长按3：重新请求当前模式
        event::on_target(EventType::KeyLongEnd(3), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.fetch().await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
    }
}

async fn fetch_stock(mode: ChartMode, code: &str, name: &str) -> Result<Box<StockData>, &'static str> {
    let stack = crate::wifi::use_wifi().await.map_err(|_| "wifi连接失败")?;
    crate::wifi::set_request_loading(true);
    let mut req = RequestClient::new(stack).await;

    let out = if mode == ChartMode::Quote {
        // 实时行情：腾讯 qt.gtimg.cn，无需 Referer
        let url = stock::build_quote_url(code);
        let result = req.send_request_slice(url.as_str()).await;
        crate::wifi::set_request_loading(false);
        match result {
            Ok(data) => parse_quote(data, code, name).ok_or("解析失败"),
            Err(e) => { println!("stock quote request err: {:?}", e); Err(reason_of(&e)) }
        }
    } else {
        let url = stock::build_url(code, mode, stock::bar_count(mode));
        let result = req.send_request_slice(url.as_str()).await;
        crate::wifi::set_request_loading(false);
        match result {
            Ok(data) => parse_kline(data, code, name, mode).ok_or("解析失败"),
            Err(e) => { println!("stock request err: {:?}", e); Err(reason_of(&e)) }
        }
    };
    crate::wifi::finish_wifi().await;
    out
}

fn reason_of(e: &RequestError) -> &'static str {
    match e {
        RequestError::TlsError(_) => "TLS握手失败",
        RequestError::DnsLookup => "DNS解析失败",
        RequestError::ConnectError(_) => "连接失败",
        RequestError::TimeOut => "请求超时",
        RequestError::BufferOver => "响应过大",
        RequestError::UnsupportedScheme => "不支持协议",
        RequestError::PortParse(_) => "端口错误",
        _ => "请求失败",
    }
}
