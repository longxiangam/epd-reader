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
    self, parse_kline, ChartMode, StockData, DEFAULT_STOCK,
};
use crate::pages::Page;
use crate::request::{RequestClient, RequestError};
use crate::sleep::{refresh_active_time, to_sleep_tips};

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

    /// 切换 分时/日K/周K/月K/折线。若新旧模式同数据源（日K↔折线），
    /// 只换渲染方式，不重新请求；否则清空数据触发重新请求。
    fn switch_mode(&mut self) {
        let old_source = self.mode.source();
        let new_mode = self.mode.next();
        let new_source = new_mode.source();
        if new_source != old_source {
            self.data = None;
        }
        self.mode = new_mode;
    }

    async fn fetch(&mut self) {
        self.loading = true;
        self.need_render = true;
        self.render().await;
        let code = DEFAULT_STOCK;
        match fetch_stock(self.mode, code).await {
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
            mode: ChartMode::Day,
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

                let data = StockRenderData {
                    w,
                    h,
                    mode: self.mode,
                    data: self.data.as_deref(),
                    loading: self.loading,
                    err_msg: self.err_msg,
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
        // 进入即拉取一次当前模式
        self.fetch().await;
        loop {
            if !self.running {
                break;
            }
            refresh_active_time().await;
            if self.need_render {
                self.render().await;
            }
            to_sleep_tips(Duration::from_secs(0), Duration::from_secs(30), true).await;
            Timer::after(Duration::from_millis(50)).await;
        }
        crate::display::set_sleep_renderer(None);
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        // 短按1：刷新（重新请求当前模式）
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.fetch().await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 短按2：切换 分时/日K/周K/月K/折线
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                mut_ref.switch_mode();
                if mut_ref.data.is_none() {
                    mut_ref.fetch().await;
                } else {
                    mut_ref.need_render = true;
                }
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            })
        }).await;
        // 短按3：退出
        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                mut_ref.back().await;
            })
        }).await;
    }
}

async fn fetch_stock(mode: ChartMode, code: &str) -> Result<Box<StockData>, &'static str> {
    let stack = crate::wifi::use_wifi().await.map_err(|_| "wifi连接失败")?;
    println!("[stock] heap free @wifi up: {}", esp_alloc::HEAP.free());
    crate::wifi::set_request_loading(true);
    let mut req = RequestClient::new(stack).await;
    let url = stock::build_url(code, mode, stock::bar_count(mode));
    // 就地解析：send_request_slice 直接返回静态 RESPONSE_BUF 的切片，不 .to_vec() 拷贝，
    // 避免每次请求 alloc/free 6.7KB 拷贝块导致堆碎片化。
    let result = req.send_request_slice(url.as_str()).await;
    crate::wifi::set_request_loading(false);
    let out = match result {
        Ok(data) => {
            println!("[stock] resp len: {} heap free: {}", data.len(), esp_alloc::HEAP.free());
            parse_kline(data, code, mode).ok_or("解析失败")
        }
        Err(e) => {
            println!("stock request err: {:?}", e);
            Err(reason_of(&e))
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
