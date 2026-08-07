use crate::model::stock::{ChartMode, StockData, StockMinuteCache};
use crate::storage::StockStorage;

pub struct StockRenderData<'a> {
    pub w: i32,
    pub h: i32,
    pub mode: ChartMode,
    pub data: Option<&'a StockData>,
    pub loading: bool,
    pub err_msg: Option<&'static str>,
    pub battery_percent: Option<u32>,
    pub wifi_connected: bool,
    pub wifi_connecting: bool,
    pub request_loading: bool,
}

/// 总览页渲染数据：6 支股票的分时缓存（rtc_fast）网格。
pub struct OverviewRenderData<'a> {
    pub w: i32,
    pub h: i32,
    pub cursor: usize,
    pub storage: &'a StockStorage,
    /// 6 支当日分时缓存（closes[48] + 昨收 + last_price + n_bars）。
    pub cache: &'a [StockMinuteCache; 6],
    pub loading: bool,
    pub battery_percent: Option<u32>,
    pub wifi_connected: bool,
    pub wifi_connecting: bool,
    pub request_loading: bool,
}
