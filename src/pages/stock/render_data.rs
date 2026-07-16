use crate::model::stock::{ChartMode, StockData};

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
