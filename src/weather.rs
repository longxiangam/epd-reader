use alloc::format;
use esp_hal::macros::ram;
use esp_println::println;
use futures::future::err;
use crate::model::seniverse::{DailyResult, form_json};
use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};
use crate::worldtime::{get_clock};

use crate::model::holiday::{HolidayResponse, form_json as form_holiday_json, form_json_each};
use crate::storage::{HolidayStorage, NvsStorage, WeatherStorage};
pub struct Weather{
}

impl Weather {

    fn new()->Self{
        Self{
        }
    }
    pub async fn request()->Result<(),()> {
        let stack = use_wifi().await;
        match stack {
            Ok(v) => {
                println!("请求 stack 成功");
                let mut request = RequestClient::new(v).await;
                println!("开始请求成功");
                let result = request.send_request("http://api.seniverse.com/v3/weather/daily.json?key=SvRIiZPU5oGiqcHc1&location=wuhan&language=zh-Hans&unit=c&start=0&days=5").await;
                match result {
                    Ok(response) => {
                        let mut daily_result = form_json(&response.data[..response.length]);
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
  
    pub async fn sync_weather(){
        
        if unsafe{WEATHER_SYNC_SECOND} == 0 {
            Self::get_weather().await;//加载同步时间
        }

        let now_sec =  get_clock().unwrap().now().await.unix_timestamp() as u64;
        // 天气：5小时刷新一次
        if !sync_weather_success() || now_sec - unsafe { WEATHER_SYNC_SECOND } > 3600 * 5 {
            let _ = Weather::request().await;
        }
    }
    pub async fn get_weather()->Option<DailyResult>{
        let weather_storage = WeatherStorage::read().unwrap();
        unsafe {
            WEATHER_SYNC_SECOND = weather_storage.sync_time_second;
        }
        //判断时间
        weather_storage.weather_data
    } 
    
    pub async fn save(daily_result: DailyResult){
        unsafe {
            WEATHER_SYNC_SECOND = get_clock().unwrap().now().await.unix_timestamp() as u64;
        }
        
        let mut weather_storage = WeatherStorage::read().unwrap();
        weather_storage.weather_data =  Some(daily_result);
        weather_storage.sync_time_second =  unsafe{WEATHER_SYNC_SECOND};
        weather_storage.write().unwrap();
    }
   
}


#[ram(rtc_fast)]
pub static mut WEATHER_SYNC_SECOND:u64   =  0;
pub static mut WEATHER_SYNC_SECOND_ERROR_SECOND:u64  = 0;

pub fn sync_weather_success()->bool{
    unsafe {
        WEATHER_SYNC_SECOND > 0
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
                // 这里请替换为实际的节假日API地址
                let result = request.send_request(format!("https://api.jiejiariapi.com/v1/holidays/{}",current_year).as_str()).await;
            
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

        if unsafe{HOLIDAY_SYNC_SECOND} == 0 {
            Self::get_holiday().await;//加载同步时间
        }


        let current_second = get_clock().unwrap().now().await.unix_timestamp() as u64;
        let current_year =  get_clock().unwrap().now().await.year() as u32;
        if !sync_holiday_success()  || unsafe { HOLIDAY_SYNC_YEAR !=  current_year } {
            let error_second = unsafe{ HOLIDAY_SYNC_ERROR_SECOND };
            if error_second ==0 || current_second - error_second > 60 {
                let mut try_times = 3;
                loop{
                    if let Ok(v) = HolidayInfo::request().await {
                        break;
                    }else{
                        try_times-=1;
                        if try_times == 0 {
                            unsafe{HOLIDAY_SYNC_ERROR_SECOND =  current_second};
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
            HOLIDAY_SYNC_SECOND = holiday_storage.sync_time_second;
            if let Some(ref temp) = holiday_storage.holiday_response{
                HOLIDAY_SYNC_YEAR = temp.year;
            }
        }
        //判断时间
        holiday_storage.holiday_response
    }

    pub async fn save(holiday_response: HolidayResponse){
        unsafe {
            HOLIDAY_SYNC_SECOND = get_clock().unwrap().now().await.unix_timestamp() as u64;
        }

        println!("HOLIDAY_SYNC_SECOND:{}", unsafe{HOLIDAY_SYNC_SECOND});

        let mut holiday_storage = HolidayStorage::read().unwrap();
        holiday_storage.holiday_response =  Some(holiday_response);
        holiday_storage.sync_time_second =  unsafe{HOLIDAY_SYNC_SECOND};
        holiday_storage.write().unwrap();
    }


}


#[ram(rtc_fast)]
pub static mut HOLIDAY_SYNC_SECOND: u64 = 0;
#[ram(rtc_fast)]
pub static mut HOLIDAY_SYNC_YEAR: u32 = 0;

pub static mut HOLIDAY_SYNC_ERROR_SECOND: u64 = 0;

pub fn sync_holiday_success() -> bool {
    unsafe { HOLIDAY_SYNC_SECOND > 0 }
}







