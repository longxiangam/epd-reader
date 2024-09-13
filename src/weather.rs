
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_println::println;
use crate::make_static;
use crate::model::seniverse::{DailyResult, form_json};
use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};



pub struct Weather{
   pub daily_result:Mutex<CriticalSectionRawMutex,Option<DailyResult>>
}

impl Weather {

    fn new()->Self{
        Self{
            daily_result: Mutex::new(None),
        }
    }
   pub async fn request(& self)->Result<(),()>{
        let stack = use_wifi().await;
        if let Ok(v) = stack {
            println!("请求 stack 成功");
            let mut request = RequestClient::new(v).await;
            println!("开始请求成功");
            let result = request.send_request("http://api.seniverse.com/v3/weather/daily.json?key=SvRIiZPU5oGiqcHc1&location=wuhan&language=zh-Hans&unit=c&start=0&days=5").await;
            match result {
                Ok(response) => {
                    finish_wifi().await;
                    let mut daily_result = form_json(&response.data[..response.length]);
                    if let Some(mut v) =  daily_result {
                        self.daily_result.lock().await.replace(v.results.pop().unwrap());
                    }
                    println!("请求成功{}", core::str::from_utf8(& response.data[..response.length]).unwrap());
                    Ok(())
                }
                Err(e) => {
                    finish_wifi().await;
                    println!("请求失败{:?}",e);
                    Err(())
                }
            }
        }else{
            Err(())
        }
    }

}

pub static mut WEATHER: Option<&'static  Weather>  =  None;
pub static WEATHER_SYNC_SUCCESS:Mutex<CriticalSectionRawMutex,bool>   =  Mutex::new(false);

pub fn get_weather() -> Option<&'static  Weather> {
    unsafe {
        return WEATHER;
    }
}


#[embassy_executor::task]
pub async fn weather_worker() {
    let weather = make_static!(Weather,Weather::new());
    unsafe {
        WEATHER.replace(weather);
    }
    let mut sleep_sec =  3600;
    loop {


         match get_weather().unwrap().request().await {
             Ok(stack) =>{
                 *WEATHER_SYNC_SUCCESS.lock().await = true;
                 sleep_sec = 3600;
             }
             Err(e) => {
                 sleep_sec = 5;
             }
         }


        embassy_time::Timer::after(embassy_time::Duration::from_secs(sleep_sec)).await;
    }
}

