use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{ Duration, Instant};
use esp_hal::peripherals::LPWR;
use esp_hal::{delay::Delay, rtc_cntl::Rtc};
use esp_hal::gpio::{GpioPin, Output, RtcPinWithResistors};
use esp_hal::macros::ram;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeSource, WakeupLevel};
use esp_println::println;
use log::info;
use heapless::Vec;

use crate::CLOCKS_REF;
use crate::wifi::{force_stop_wifi, STOP_WIFI_SIGNAL};
use crate::worldtime::save_time_to_rtc;

pub static RTC_MANGE:Mutex<CriticalSectionRawMutex,Option<Rtc>> = Mutex::new(None);
pub static LAST_ACTIVE_TIME:Mutex<CriticalSectionRawMutex,Instant> = Mutex::new(Instant::MAX);
pub static mut WAKEUP_PINS:  Vec<(&'static mut dyn RtcPinWithResistors, WakeupLevel),5> = Vec::new();

pub static mut EINK_PWER_PIN:Mutex<CriticalSectionRawMutex,Option<Output<GpioPin<21>>>> = Mutex::new(None);
pub static mut SD_PWER_PIN:Mutex<CriticalSectionRawMutex,Option<Output<GpioPin<1>>>> = Mutex::new(None);

#[ram(rtc_fast)]
static mut WHEN_SLEEP_RTC_MS:u64 = 0;

pub async fn refresh_active_time(){
     *LAST_ACTIVE_TIME.lock().await = Instant::now();
}

pub async fn to_sleep(sleep_time:Duration,idle_time:Duration) {
    to_sleep_tips(sleep_time,idle_time,false).await;
}

pub async fn to_sleep_tips(sleep_time:Duration,idle_time:Duration,show_sleep:bool) {
    if Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) > idle_time  {
        //不关wifi,唤醒时运行到wifi部分会卡着
        force_stop_wifi().await;

        let wakeup_pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] = unsafe{ WAKEUP_PINS.as_mut() };
        let rtcio = RtcioWakeupSource::new(wakeup_pins);

        let mut  wakeup_source =TimerWakeupSource::new(core::time::Duration::from_micros(sleep_time.as_micros()));

        let mut ws:Vec<& dyn WakeSource,2> = Vec::new();
        ws.push(&rtcio);
        if sleep_time.as_ticks() > 0{
            ws.push(&wakeup_source);
        }
        if show_sleep {
            crate::display::show_sleep().await;
        }

        unsafe {
            WHEN_SLEEP_RTC_MS = get_rtc_ms().await;

            EINK_PWER_PIN.lock().await.take().unwrap().set_high();
            SD_PWER_PIN.lock().await.take().unwrap().set_high();
        }

        save_time_to_rtc().await;


        let mut delay = Delay::new(unsafe{CLOCKS_REF.unwrap()});
        RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());

    }
}
pub async fn get_rtc_ms()->u64{
    RTC_MANGE.lock().await.as_mut().unwrap().get_time_ms()
}
pub async fn get_sleep_ms()->u64{
    get_rtc_ms().await - unsafe{WHEN_SLEEP_RTC_MS}
}

pub async fn add_rtcio(rtcpin:&'static mut dyn RtcPinWithResistors, wakeup_level: WakeupLevel){
    unsafe {
        WAKEUP_PINS.push((rtcpin,wakeup_level));
    }
}