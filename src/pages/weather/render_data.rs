use time::OffsetDateTime;
use crate::model::seniverse::DailyResult;

pub struct WeatherRenderData<'a> {
    pub w: i32,
    pub h: i32,
    pub current_date: Option<OffsetDateTime>,
    pub battery_percent: Option<u32>,
    pub wifi_connected: bool,
    pub wifi_connecting: bool,
    pub request_loading: bool,
    pub weather: Option<&'a DailyResult>,
    pub weather_synced: bool,
    pub holiday_synced: bool,
    pub time_synced: bool,
}
