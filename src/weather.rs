
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::macros::ram;
use esp_println::println;
use crate::{display, make_static};
use crate::model::seniverse::{DailyResult, form_json};
use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};
use crate::worldtime::{get_clock, sync_time_success, CLOCK_SYNC_TIME_SECOND};

pub struct Weather{
   pub daily_result:Mutex<CriticalSectionRawMutex,Option<DailyResult>>
}

impl Weather {

    fn new()->Self{
        Self{
            daily_result: Mutex::new(None),
        }
    }
   pub async fn request(& self)->Result<(),()> {
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
                           self.daily_result.lock().await.replace(v.results.pop().unwrap());
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
               println!("请求stack 失败,{:?}",e);
               Err(())
           }
       }
   }
}

#[ram(rtc_fast)]
pub static mut WEATHER: Option<Weather>  =  None;
#[ram(rtc_fast)]
pub static mut WEATHER_SYNC_SECOND:u64   =  0;

pub fn get_weather() -> Option<&'static  Weather> {
    unsafe {
        return WEATHER.as_ref();
    }
}
pub fn sync_weather_success()->bool{
    unsafe {
        WEATHER_SYNC_SECOND > 0
    }
}


#[embassy_executor::task]
pub async fn weather_worker() {
   
    unsafe {
        if WEATHER.is_none() {
            let weather = Weather::new();
            WEATHER.replace(weather);
        }
    }

    let mut sleep_sec =  3600;
    loop {
       
        loop {
            //待时间完成
            if sync_time_success() {
                break;
            } else {
                embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
            }
        }

        let now_sec =  get_clock().unwrap().now().await.unix_timestamp() as u64;
        if !sync_weather_success() ||  now_sec - unsafe {WEATHER_SYNC_SECOND} > 3600 * 5 {
            match get_weather().unwrap().request().await {
                Ok(()) => {
                    unsafe {
                        WEATHER_SYNC_SECOND = get_clock().unwrap().now().await.unix_timestamp() as u64;
                    }
                    sleep_sec = 3600;
                }
                Err(e) => {
                    sleep_sec = 5;
                }
            }

        }else{
            sleep_sec = 5;
        }

        embassy_time::Timer::after(embassy_time::Duration::from_secs(sleep_sec)).await;
    }
}

