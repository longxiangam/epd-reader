use alloc::format;
use esp_hal::ram;
#[allow(internal_features)]
use core::ptr::{addr_of, addr_of_mut};
use esp_println::println;
use crate::model::seniverse::{DailyResult, form_json};
use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};
use crate::worldtime::{get_clock};

use crate::model::holiday::{HolidayResponse, form_json_each};
use crate::storage::{HolidayStorage, NvsStorage, WeatherStorage};
pub struct Weather{
}

impl Weather {

    fn new()->Self{
        Self{
        }
    }
    pub async fn request()->Result<(),()> {
        let weather_storage = WeatherStorage::read().unwrap_or_default();

        #[cfg(not(feature = "weather-openmeteo"))]
        { Self::request_seniverse(&weather_storage).await }

        #[cfg(feature = "weather-openmeteo")]
        { Self::request_open_meteo(&weather_storage).await }
    }

    #[cfg(not(feature = "weather-openmeteo"))]
    async fn request_seniverse(storage: &WeatherStorage)->Result<(),()> {
        let api_key = if storage.token.is_empty() {
            "SvRIiZPU5oGiqcHc1"
        } else {
            storage.token.as_str()
        };
        let city = if storage.city.is_empty() {
            "wuhan"
        } else {
            storage.city.as_str()
        };

        let url = format!("http://api.seniverse.com/v3/weather/daily.json?key={}&location={}&language=zh-Hans&unit=c&start=0&days=5", api_key, city);

        let stack = use_wifi().await;
        match stack {
            Ok(v) => {
                println!("请求 stack 成功");
                let mut request = RequestClient::new(v).await;
                crate::wifi::set_request_loading(true);
                let result = request.send_request(url.as_str()).await;
                crate::wifi::set_request_loading(false);
                match result {
                    Ok(response) => {
                        let daily_result = form_json(&response.data[..response.length]);
                        if let Some(mut v) = daily_result {
                            Self::save(v.results.pop().unwrap()).await;
                        }
                        println!("请求成功{}", core::str::from_utf8(&response.data[..response.length]).unwrap());
                        finish_wifi().await;
                        Ok(())
                    }
                    Err(e) => {
                        finish_wifi().await;
                        println!("请求失败{:?}", e);
                        Err(())
                    }
                }
            }
            Err(e) => {
                finish_wifi().await;
                println!("请求stack 失败,{:?}",e);
                Err(())
            }
        }
    }

    #[cfg(feature = "weather-openmeteo")]
    async fn request_open_meteo(storage: &WeatherStorage)->Result<(),()> {
        // city 字段存储坐标，兼容 "纬度:经度"（自动定位）与 "纬度,经度" 两种格式
        let (lat, lon) = if storage.city.is_empty() {
            ("30.5928", "114.3055")
        } else {
            match storage
                .city
                .as_str()
                .split_once(':')
                .or_else(|| storage.city.as_str().split_once(','))
            {
                Some((la, lo)) => (la, lo),
                None => ("30.5928", "114.3055"),
            }
        };

        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,relative_humidity_2m_mean,wind_speed_10m_max,wind_direction_10m_dominant&timezone=auto&forecast_days=5",
            lat, lon
        );

        let stack = use_wifi().await;
        match stack {
            Ok(v) => {
                println!("Open-Meteo stack 成功");
                let mut request = RequestClient::new(v).await;
                let result = request.send_request(url.as_str()).await;
                match result {
                    Ok(response) => {
                        let daily_result = crate::model::open_meteo::parse_json(&response.data[..response.length]);
                        if let Some(v) = daily_result {
                            Self::save(v).await;
                        }
                        println!("Open-Meteo 请求成功");
                        finish_wifi().await;
                        Ok(())
                    }
                    Err(e) => {
                        finish_wifi().await;
                        println!("Open-Meteo 请求失败{:?}", e);
                        Err(())
                    }
                }
            }
            Err(e) => {
                finish_wifi().await;
                println!("Open-Meteo stack 失败,{:?}",e);
                Err(())
            }
        }
    }
  
    pub async fn sync_weather(){

        if unsafe { *addr_of!(WEATHER_SYNC_SECOND) } == 0 {
            Self::get_weather().await;
        }

        let now_sec = get_clock().unwrap().now().await.unix_timestamp() as u64;
        if !sync_weather_success() || now_sec - unsafe { *addr_of!(WEATHER_SYNC_SECOND) } > 3600 * 5 {
            let _ = Weather::request().await;
        }
    }
    pub async fn get_weather()->Option<DailyResult>{
        let weather_storage = WeatherStorage::read().unwrap();
        unsafe {
            *addr_of_mut!(WEATHER_SYNC_SECOND) = weather_storage.sync_time_second;
        }
        //判断时间
        weather_storage.weather_data
    } 
    
    #[inline(always)]
    pub async fn save(daily_result: DailyResult){
        unsafe {
            *addr_of_mut!(WEATHER_SYNC_SECOND) = get_clock().unwrap().now().await.unix_timestamp() as u64;
        }

        let mut weather_storage = WeatherStorage::read().unwrap_or_default();
        weather_storage.weather_data =  Some(daily_result);
        weather_storage.sync_time_second =  unsafe{ *addr_of!(WEATHER_SYNC_SECOND) };
        weather_storage.write().unwrap();
    }
   
}


#[ram(unstable(rtc_fast))]
static mut WEATHER_SYNC_SECOND:u64 = 0;
static mut WEATHER_SYNC_SECOND_ERROR_SECOND:u64 = 0;

pub fn sync_weather_success()->bool{
    unsafe {
        *addr_of!(WEATHER_SYNC_SECOND) > 0
    }
}



pub struct HolidayInfo{
}

impl HolidayInfo {
    fn new() -> Self {
        Self {
        }
    }
    pub async fn request() -> Result<(), ()> {
        let stack = use_wifi().await;
        match stack {
            Ok(v) => {
                println!("请求 stack 成功 (holiday)");
                let mut request = RequestClient::new(v).await;
                println!("开始请求节假日");
                let current_year =  get_clock().unwrap().now().await.year() as u32;
                crate::wifi::set_request_loading(true);
                let result = request.send_request(format!("https://api.jiejiariapi.com/v1/holidays/{}",current_year).as_str()).await;
                crate::wifi::set_request_loading(false);
            
                match result {
                    Ok(response) => {
                        let holiday_result = form_json_each(&response.data[..response.length]);

                        if let Some(mut v) = holiday_result {
                            v.year = current_year;
                            Self::save(v).await;
                        }
                        
                        println!("节假日请求成功{}", core::str::from_utf8(&response.data[..response.length]).unwrap());
                        finish_wifi().await;
                        Ok(())
                    }
                    Err(e) => {
                        finish_wifi().await;
                        println!("节假日请求失败{:?}", e);
                        Err(())
                    }
                }
            }
            Err(e) => {
                println!("请求stack 失败 (holiday),{:?}", e);
                finish_wifi().await;
                Err(())
            }
        }
    }


    pub async fn sync_holiday(){

        if unsafe { *addr_of!(HOLIDAY_SYNC_SECOND) } == 0 {
            Self::get_holiday().await;
        }


        let current_second = get_clock().unwrap().now().await.unix_timestamp() as u64;
        let current_year =  get_clock().unwrap().now().await.year() as u32;
        if !sync_holiday_success() || unsafe { *addr_of!(HOLIDAY_SYNC_YEAR) != current_year } {
            let error_second = unsafe { *addr_of!(HOLIDAY_SYNC_ERROR_SECOND) };
            if error_second == 0 || current_second - error_second > 60 {
                let mut try_times = 3;
                loop{
                    if let Ok(_v) = HolidayInfo::request().await {
                        break;
                    }else{
                        try_times-=1;
                        if try_times == 0 {
                            unsafe { *addr_of_mut!(HOLIDAY_SYNC_ERROR_SECOND) = current_second; }
                            break;
                        }
                    }

                }
            }
        }
    }
    pub async fn get_holiday()->Option<HolidayResponse>{
        let holiday_storage = HolidayStorage::read().unwrap();
        unsafe {
            *addr_of_mut!(HOLIDAY_SYNC_SECOND) = holiday_storage.sync_time_second;
            if let Some(ref temp) = holiday_storage.holiday_response{
                *addr_of_mut!(HOLIDAY_SYNC_YEAR) = temp.year;
            }
        }
        //判断时间
        holiday_storage.holiday_response
    }

    #[inline(always)]
    pub async fn save(holiday_response: HolidayResponse){
        unsafe {
            *addr_of_mut!(HOLIDAY_SYNC_SECOND) = get_clock().unwrap().now().await.unix_timestamp() as u64;
        }

        println!("HOLIDAY_SYNC_SECOND:{}", unsafe { *addr_of!(HOLIDAY_SYNC_SECOND) });

        let mut holiday_storage = HolidayStorage::default();
        holiday_storage.holiday_response =  Some(holiday_response);
        holiday_storage.sync_time_second =  unsafe { *addr_of!(HOLIDAY_SYNC_SECOND) };
        holiday_storage.write().unwrap();
    }


}


#[ram(unstable(rtc_fast))]
pub(crate) static mut HOLIDAY_SYNC_SECOND: u64 = 0;
#[ram(unstable(rtc_fast))]
static mut HOLIDAY_SYNC_YEAR: u32 = 0;

static mut HOLIDAY_SYNC_ERROR_SECOND: u64 = 0;

pub fn sync_holiday_success() -> bool {
    unsafe { *addr_of!(HOLIDAY_SYNC_SECOND) > 0 }
}







