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
use time::OffsetDateTime;

/// 股票当前图模式，存 rtc_fast，跨深睡重启保留。
/// 深睡唤醒会重启程序，若不保留则模式被重置为默认 Day，分时的 2 分钟周期就失效了。
#[ram(unstable(rtc_fast))]
static mut STOCK_MODE: u8 = 4; // 默认分时（Minute=4）

/// 股票当前视图：0=总览，1=明细。存 rtc_fast，深睡唤醒后恢复到上次视图。
#[ram(unstable(rtc_fast))]
static mut STOCK_VIEW: u8 = 0;

/// 上次总览拉取的 RTC 毫秒时间戳。跨深睡保留，用于判断是否跳过重复拉取。
#[ram(unstable(rtc_fast))]
static mut STOCK_LAST_FETCH_MS: u64 = 0;

/// 6 支股票当日分时缓存（rtc_fast，跨深睡保留）：close[48] 按日内槽位 + 真实昨收。
/// 总览页据此做增量拉取（只取上次之后的根数并合并）与累积渲染（图表不清空），
/// 昨收只在首次/跨日查一次实时行情。约 1.3KB（8KB rtc_fast 内）。
#[ram(unstable(rtc_fast))]
static mut STOCK_MINUTE_CACHE: [stock::StockMinuteCache; 6] = [stock::StockMinuteCache::ZERO; 6];

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StockView {
    Overview,
    Detail,
}

pub struct StockPage {
    pub(crate) running: bool,
    pub(crate) need_render: bool,
    /// 回调置位、run 循环消费：拉取数据（fetch 移出回调，避免阻塞按键）。
    pub(crate) need_fetch: bool,
    /// 强制走网络拉取（跳过缓存），长按 Key1 手动刷新时置位。
    pub(crate) force_fetch: bool,
    /// 回调置位、run 循环消费：用全刷 LUT 重绘当前数据（同源切换，如 Day↔折线）。
    pub(crate) need_clean_render: bool,
    pub(crate) mode: ChartMode,
    pub(crate) view: StockView,
    /// 总览页选中格 0..count。
    pub(crate) cursor: usize,
    pub(crate) data: Option<Box<StockData>>,
    pub(crate) loading: bool,
    pub(crate) err_msg: Option<&'static str>,
}

impl StockPage {
    async fn back(&mut self) {
        // 退出到主界面：重置视图为总览，下次进入从总览开始
        unsafe { *core::ptr::addr_of_mut!(STOCK_VIEW) = 0; }
        self.running = false;
    }

    /// 切换 分时/日K/周K/月K/折线（forward=true 下一个，false 上一个）。
    /// 返回是否跨数据源（跨源需重新请求；同源如 日K↔折线 只换渲染方式）。
    /// 不清空 data：拉取期间保留旧数据用于显示（参考天气页），新数据回来后再替换。
    fn switch_mode(&mut self, forward: bool) -> bool {
        let old_source = self.mode.source();
        let new_mode = if forward { self.mode.next() } else { self.mode.prev() };
        let new_source = new_mode.source();
        self.mode = new_mode;
        unsafe { *core::ptr::addr_of_mut!(STOCK_MODE) = new_mode.encode(); }
        new_source != old_source
    }

    /// 切换查询的股票（长按1/2）。更新 StockStorage.selected；返回是否实际切换。
    /// 不清空 data：保留旧股票数据用于拉取期间显示。
    fn switch_stock(&mut self, forward: bool) -> bool {
        let mut ss = crate::storage::StockStorage::read().unwrap_or_default();
        if ss.count > 1 {
            let c = ss.count as usize;
            let cur = (ss.selected as usize).min(c - 1);
            let nxt = if forward { (cur + 1) % c } else { (cur + c - 1) % c };
            ss.selected = nxt as u8;
            let _ = ss.write();
            true
        } else {
            false
        }
    }

    /// 已配置的股票数量（0..=6）。
    fn overview_count(&self) -> usize {
        (crate::storage::StockStorage::read().unwrap_or_default().count as usize).min(6)
    }

    /// 总览页移动选中格（夹 0..count）。
    fn overview_move_cursor(&mut self, forward: bool) {
        let c = self.overview_count();
        if c == 0 {
            return;
        }
        self.cursor = if forward {
            (self.cursor + 1) % c
        } else {
            (self.cursor + c - 1) % c
        };
        self.need_render = true;
    }

    /// 从总览进入明细：selected=cursor，切 Detail，按 STOCK_MODE 恢复模式，触发拉取。
    fn enter_detail(&mut self) {
        let c = self.overview_count();
        if c == 0 {
            return;
        }
        if self.cursor >= c {
            self.cursor = c - 1;
        }
        let mut ss = crate::storage::StockStorage::read().unwrap_or_default();
        ss.selected = self.cursor as u8;
        let _ = ss.write();
        self.mode = ChartMode::decode(unsafe { *core::ptr::addr_of!(STOCK_MODE) });
        self.view = StockView::Detail;
        unsafe { *core::ptr::addr_of_mut!(STOCK_VIEW) = 1; }
        self.data = None;
        self.err_msg = None;
        self.need_fetch = true;
    }

    /// 明细返回总览：复用已缓存 overview，不重拉。
    fn back_to_overview(&mut self) {
        self.view = StockView::Overview;
        unsafe { *core::ptr::addr_of_mut!(STOCK_VIEW) = 0; }
        // cursor 恢复到 selected（刚从该支明细返回）
        let ss = crate::storage::StockStorage::read().unwrap_or_default();
        let c = (ss.count as usize).min(6);
        self.cursor = if c > 0 { (ss.selected as usize).min(c - 1) } else { 0 };
        self.need_render = true;
    }

    /// 刷新总览：按 rtc_fast 缓存做全量/增量。
    /// 首次/跨日/换股/时间未同步 → 全量分时 + 查一次昨收；
    /// 同日 → 只拉上次之后的根数合并进缓存（昨收复用，不查行情）。失败保留旧数据，下次唤醒重试。
    async fn fetch_overview(&mut self) {
        crate::wifi::set_request_loading(true);
        self.loading = true;
        self.need_render = true;
        self.render().await;

        let storage = crate::storage::StockStorage::read().unwrap_or_default();
        let count = (storage.count as usize).min(6);
        println!("[stock] storage.count={} entries:", count);
        for j in 0..count {
            println!("  [{}] code={} name={}", j, storage.entries[j].code.as_str(), storage.entries[j].name.as_str());
        }

        // 当前时间（跨日判断 + 增量槽位）。无时钟或年份<2024 视为未同步 → 回退全量。
        let now: Option<OffsetDateTime> = match crate::worldtime::get_clock() {
            Some(c) => Some(c.local().await),
            None => None,
        };
        let time_ok = now.as_ref().map(|n| n.year() >= 2024).unwrap_or(false);
        let today = now.as_ref().map(stock::today_yyyymmdd).unwrap_or(0);

        for i in 0..count {
            let code = storage.entries[i].code.as_str();
            let name = storage.entries[i].name.as_str();
            // 跳过空代码（web 配置中留空的行，或 NVS 数据偏移错位导致的垃圾）
            if code.is_empty() {
                continue;
            }
            // SAFETY: 单核协作式调度；fetch_overview 仅在 run() 循环执行，无重入。
            let cache: &mut stock::StockMinuteCache = unsafe {
                &mut (*core::ptr::addr_of_mut!(STOCK_MINUTE_CACHE))[i]
            };
            let same_day = time_ok && cache.matches_code(code) && cache.date == today;
            println!("[stock] i={} code={} same_day={} time_ok={} today={} cache.date={} cache.n_bars={}", i, code, same_day, time_ok, today, cache.date, cache.n_bars);
            if !same_day {
                // 全量 + 取昨收
                cache.reset();
                cache.set_code(code);
                if let Ok(d) = fetch_stock(ChartMode::Minute, code, name, stock::MINUTE_SLOTS).await {
                    if time_ok {
                        cache.date = today; // 取到分时才标记当日（失败则下次唤醒重试全量）
                    }
                    let nk = d.klines.len();
                    // 诊断：前 3 根 bar 的时间戳→槽位
                    for (idx, k) in d.klines.iter().take(3).enumerate() {
                        println!("  bar[{}] date={} slot={} close={}", idx, k.date, stock::bar_slot(k.date), k.close);
                    }
                    stock::merge_minute(cache, &d.klines);
                    println!("  full: nk={} n_bars={} last_price={}", nk, cache.n_bars, cache.last_price);
                    // 缓存前 5 槽值（诊断数据是否正确存入）
                    if cache.n_bars >= 5 {
                        println!("  closes[0..5]: {:.2} {:.2} {:.2} {:.2} {:.2}",
                            cache.closes[0], cache.closes[1], cache.closes[2], cache.closes[3], cache.closes[4]);
                    }
                    // 昨收：首次/跨日只查一次实时行情（真实昨收，与手机涨跌幅一致）
                    if let Ok(q) = fetch_stock(ChartMode::Quote, code, name, 0).await {
                        cache.preclose = q.preclose;
                        if q.last_price > 0.0 {
                            cache.last_price = q.last_price;
                        }
                        println!("  quote: preclose={} last_price={}", cache.preclose, cache.last_price);
                    }
                }
            } else if let Some(n) = now.as_ref() && cache.n_bars > 0 {
                // 同日增量：只拉 cur_slot - last_slot 根，合并；昨收复用缓存。
                // n_bars==0 的首次场景已在上面全量分支处理；盘后/盘前 now_slot=0/47，
                // need 用 MINUTE_SLOTS 保证拉够（merge 按槽覆盖，冗余无害）。
                let cur_slot = stock::now_slot(n);
                let last_slot = ((cache.n_bars as i32) - 1).max(0);
                let need = if cur_slot as i32 <= last_slot {
                    stock::MINUTE_SLOTS // 盘后/盘前：拉全量，确保缓存覆盖完整
                } else {
                    ((cur_slot as i32) - last_slot) as usize
                };
                println!("  incr: cur_slot={} last_slot={} need={}", cur_slot, last_slot, need);
                if let Ok(d) = fetch_stock(ChartMode::Minute, code, name, need).await {
                    let nk = d.klines.len();
                    for (idx, k) in d.klines.iter().take(3).enumerate() {
                        println!("  bar[{}] date={} slot={} close={}", idx, k.date, stock::bar_slot(k.date), k.close);
                    }
                    stock::merge_minute(cache, &d.klines);
                    println!("  incr done: nk={} n_bars={} last_price={}", nk, cache.n_bars, cache.last_price);
                    if cache.n_bars >= 5 {
                        println!("  closes[0..5]: {:.2} {:.2} {:.2} {:.2} {:.2}",
                            cache.closes[0], cache.closes[1], cache.closes[2], cache.closes[3], cache.closes[4]);
                    }
                }
            }
        }
        crate::wifi::set_request_loading(false);
        self.loading = false;
        self.need_render = true;
    }

    /// 拉取当前模式数据。先整屏重绘显示加载状态（有旧数据则显示旧图表 + 加载图标，
    /// 参考天气页；无数据——如深睡唤醒/首次进入——则空白 + 加载图标），完成后整屏重绘新数据。
    async fn fetch(&mut self) {
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
        // 分时模式优先复用总览 rtc_fast 缓存（免去网络拉取，秒进）；force_fetch 时跳过
        if self.mode.is_minute() && !self.force_fetch && code_storage.count > 0 {
            let si = (code_storage.selected as usize).min((code_storage.count as usize).saturating_sub(1));
            let cache: &stock::StockMinuteCache = unsafe {
                &(*core::ptr::addr_of!(STOCK_MINUTE_CACHE))[si]
            };
            if let Some(d) = stock::stock_data_from_cache(cache, code, name) {
                println!("[stock] detail minute from cache: n_bars={} last_price={}", cache.n_bars, cache.last_price);
                self.data = Some(d);
                self.err_msg = None;
                crate::wifi::set_request_loading(false);
                self.loading = false;
                self.need_render = true;
                return;
            }
        }

        match fetch_stock(self.mode, code, name, stock::bar_count(self.mode)).await {
            Ok(d) => {
                // 分时网络拉取成功后回写缓存（保持缓存新鲜）
                if self.mode.is_minute() && code_storage.count > 0 {
                    let si = (code_storage.selected as usize).min((code_storage.count as usize).saturating_sub(1));
                    let cache: &mut stock::StockMinuteCache = unsafe {
                        &mut (*core::ptr::addr_of_mut!(STOCK_MINUTE_CACHE))[si]
                    };
                    if cache.matches_code(code) {
                        stock::merge_minute(cache, &d.klines);
                    }
                }
                self.data = Some(d);
                self.err_msg = None;
            }
            Err(msg) => {
                self.err_msg = Some(msg);
            }
        }
        // 无论成败请求都已结束：复位加载标志，避免 wifi 失败时图标残留。
        crate::wifi::set_request_loading(false);
        self.loading = false;
        self.force_fetch = false;
        // 完成后整屏重绘（run 循环消费 need_render）。
        self.need_render = true;
    }
}

impl Page for StockPage {
    fn new() -> Self {
        Self {
            running: false,
            need_render: false,
            need_fetch: false,
            force_fetch: false,
            need_clean_render: false,
            mode: ChartMode::Minute,
            view: StockView::Overview,
            cursor: 0,
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

                match self.view {
                    StockView::Overview => {
                        let storage = crate::storage::StockStorage::read().unwrap_or_default();
                        // SAFETY: 单核协作式；render 与 fetch 串行，无并发写。
                        let cache: &[stock::StockMinuteCache; 6] =
                            unsafe { &*core::ptr::addr_of!(STOCK_MINUTE_CACHE) };
                        let ord = super::render_data::OverviewRenderData {
                            w,
                            h,
                            cursor: self.cursor,
                            storage: &storage,
                            cache,
                            loading: self.loading,
                            battery_percent,
                            wifi_connected,
                            wifi_connecting,
                            request_loading,
                        };
                        let _ = super::draw_overview(display, &ord);
                    }
                    StockView::Detail => {
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
                    }
                }
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
        // 深睡唤醒恢复：STOCK_VIEW=1 时直接进明细(selected + STOCK_MODE 已跨睡眠保留)
        let saved_view = unsafe { *core::ptr::addr_of!(STOCK_VIEW) };
        if saved_view == 1 {
            // 明细：DRAM 跨重启丢失，必须拉取
            self.view = StockView::Detail;
            self.data = None;
            self.err_msg = None;
            self.fetch().await;
        } else {
            // 进入总览页：cursor 落在 selected
            self.view = StockView::Overview;
            {
                let ss = crate::storage::StockStorage::read().unwrap_or_default();
                let c = (ss.count as usize).min(6);
                self.cursor = if c > 0 { (ss.selected as usize).min(c - 1) } else { 0 };
            }
            // 距上次拉取不足 2 分钟 → 跳过，直接用 rtc_fast 缓存渲染
            let now_ms = crate::sleep::get_rtc_ms().await;
            let last_ms = unsafe { *core::ptr::addr_of!(STOCK_LAST_FETCH_MS) };
            if last_ms != 0 && now_ms.wrapping_sub(last_ms) < 120_000 {
                println!("[stock] overview skip fetch: elapsed={}ms < 120s", now_ms.wrapping_sub(last_ms));
                self.need_render = true;
            } else {
                self.fetch_overview().await;
                unsafe { *core::ptr::addr_of_mut!(STOCK_LAST_FETCH_MS) = now_ms; }
            }
        }
        loop {
            if !self.running {
                break;
            }
            // 耗时的拉取/重绘放这里执行（回调只置标志），fetch 期间按键仍可响应。
            if self.need_fetch {
                self.need_fetch = false;
                if self.view == StockView::Detail {
                    self.fetch().await;
                } else {
                    self.fetch_overview().await;
                }
            } else if self.need_clean_render {
                self.need_clean_render = false;
                crate::display::QUICKLY_LUT_CHANNEL.send(false).await;
                self.need_render = true;
                self.render().await;
                crate::display::QUICKLY_LUT_CHANNEL.send(true).await;
            }
            // 注意：此处不能再无条件 refresh_active_time()，否则空闲时间永远归零、永不睡眠。
            // 活动时间由按键（event::run 内部）刷新，空闲 30 秒后 to_sleep_tips 自动入睡。
            if self.need_render {
                self.render().await;
            }
            // 总览/分时实时模式每 2 分钟唤醒刷新；其它模式每 12 小时。
            // 深睡唤醒 = 重启 → run() 重入总览重新 fetch_overview()；
            // 模式经 rtc_fast(STOCK_MODE) 跨重启保留，故入睡时长仍正确。
            let sleep_secs: u64 = if self.view == StockView::Overview || self.mode.is_realtime() {
                120
            } else {
                12 * 3600
            };
            // 空闲多久入睡复用配置「天气睡眠时间」，与天气/日历页一致。
            let sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
            let idle_secs = if sleep_storage.weather_sleep_seconds > 0 {
                sleep_storage.weather_sleep_seconds
            } else {
                5
            };
            to_sleep_tips(Duration::from_secs(sleep_secs), Duration::from_secs(idle_secs), true).await;
            Timer::after(Duration::from_millis(50)).await;
        }
        crate::display::set_sleep_renderer(None);
    }

    async fn bind_event(&mut self) {
        event::clear().await;
        // 短按1：总览=上一格 / 明细=上一个图模式
        event::on_target(EventType::KeyShort(1), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.overview_move_cursor(false);
                } else if mut_ref.switch_mode(false) {
                    mut_ref.need_fetch = true;       // 跨源：重新请求（保留旧数据用于显示）
                } else {
                    mut_ref.need_clean_render = true; // 同源：仅重渲染
                }
            })
        }).await;
        // 短按2：总览=下一格 / 明细=下一个图模式
        event::on_target(EventType::KeyShort(2), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.overview_move_cursor(true);
                } else if mut_ref.switch_mode(true) {
                    mut_ref.need_fetch = true;
                } else {
                    mut_ref.need_clean_render = true;
                }
            })
        }).await;
        // 长按1：总览=上一格 / 明细=强制刷新（跳过缓存，走网络更新分时并回写缓存）
        event::on_target(EventType::KeyLongEnd(1), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.overview_move_cursor(false);
                } else {
                    mut_ref.force_fetch = true;
                    mut_ref.need_fetch = true;
                }
            })
        }).await;
        // 长按2：总览=下一格 / 明细=下一支股票
        event::on_target(EventType::KeyLongEnd(2), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.overview_move_cursor(true);
                } else if mut_ref.switch_stock(true) {
                    mut_ref.need_fetch = true;
                }
            })
        }).await;
        // 短按3：总览=进入明细 / 明细=重新请求当前模式
        event::on_target(EventType::KeyShort(3), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.enter_detail();
                } else {
                    mut_ref.need_fetch = true;
                }
            })
        }).await;
        // 长按3：总览=返回主界面 / 明细=返回总览（逐级返回）
        event::on_target(EventType::KeyLongEnd(3), Self::mut_to_ptr(self), move |info| {
            Box::pin(async move {
                let mut_ref: &mut Self = Self::mut_by_ptr(info.ptr).unwrap();
                if mut_ref.view == StockView::Overview {
                    mut_ref.back().await;
                } else {
                    mut_ref.back_to_overview();
                }
            })
        }).await;
    }
}

async fn fetch_stock(mode: ChartMode, code: &str, name: &str, count: usize) -> Result<Box<StockData>, &'static str> {
    let stack = crate::wifi::use_wifi().await.map_err(|e| {
        println!("stock use_wifi err: {:?}", e);
        wifi_err_msg(e)
    })?;
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
        let url = stock::build_url(code, mode, count);
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

/// use_wifi 失败的具体原因（对应 wifi.rs::WifiNetError 各分支）。
fn wifi_err_msg(e: crate::wifi::WifiNetError) -> &'static str {
    use crate::wifi::WifiNetError::*;
    match e {
        WaitConnecting => "wifi启动超时", // REINIT 后连接任务 3s 内未就绪
        TimeOut => "wifi连接超时",        // 等锁或等链路 up 超过 10s
        Infallible => "wifi未就绪",       // 未拿到网络栈（IP 尚未获取）
        Using => "wifi忙",                // 当前未产生
    }
}
