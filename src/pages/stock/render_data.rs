use crate::model::stock::{ChartMode, StockData};

pub struct StockRenderData<'a> {
    pub w: i32,
    pub h: i32,
    pub mode: ChartMode,
    pub data: Option<&'a StockData>,
    pub loading: bool,
    pub err_msg: Option<&'static str>,
}
