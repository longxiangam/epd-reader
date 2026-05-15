use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{ Duration, Instant};
use esp_hal::gpio::{Output, RtcPinWithResistors};
use esp_hal::ram;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeSource, WakeupLevel};
use heapless::Vec;

use crate::wifi::force_stop_wifi;
use crate::worldtime::save_time_to_rtc;

pub static RTC_MANGE:Mutex<CriticalSectionRawMutex,Option<esp_hal::rtc_cntl::Rtc<'static>>> = Mutex::new(None);
pub static LAST_ACTIVE_TIME:Mutex<CriticalSectionRawMutex,Instant> = Mutex::new(Instant::MAX);
static mut WAKEUP_PINS: Vec<(&'static mut dyn RtcPinWithResistors, WakeupLevel), 5> = Vec::new();

pub static EINK_PWER_PIN: Mutex<CriticalSectionRawMutex, Option<Output<'static>>> = Mutex::new(None);
pub static SD_PWER_PIN: Mutex<CriticalSectionRawMutex, Option<Output<'static>>> = Mutex::new(None);

#[ram(unstable(rtc_fast))]
static mut WHEN_SLEEP_RTC_MS:u64 = 0;

pub async fn refresh_active_time(){
     *LAST_ACTIVE_TIME.lock().await = Instant::now();
}

pub async fn to_sleep(sleep_time:Duration, idle_time:Duration) {
    to_sleep_tips(sleep_time, idle_time, false).await;
}

pub async fn to_sleep_tips(sleep_time:Duration, idle_time:Duration, show_sleep:bool) {
    if *LAST_ACTIVE_TIME.lock().await == Instant::MAX {
        return;
    }
    if Instant::now().duration_since(*LAST_ACTIVE_TIME.lock().await) > idle_time  {
        force_stop_wifi().await;

        let wakeup_pins = unsafe { core::ptr::addr_of_mut!(WAKEUP_PINS).as_mut().unwrap() };
        let rtcio = RtcioWakeupSource::new(wakeup_pins.as_mut_slice());

        let wakeup_source = TimerWakeupSource::new(core::time::Duration::from_micros(sleep_time.as_micros()));

        let mut ws: Vec<& dyn WakeSource, 2> = Vec::new();
        ws.push(&rtcio);
        if sleep_time.as_ticks() > 0 {
            ws.push(&wakeup_source);
        }
        if show_sleep {
            crate::display::show_sleep().await;
        }

        unsafe {
            *core::ptr::addr_of_mut!(WHEN_SLEEP_RTC_MS) = get_rtc_ms().await;

            EINK_PWER_PIN.lock().await.take().unwrap().set_high();
            SD_PWER_PIN.lock().await.take().unwrap().set_high();
        }

        save_time_to_rtc().await;

        RTC_MANGE.lock().await.as_mut().unwrap().sleep_deep(ws.as_slice());
    }
}
pub async fn get_rtc_ms()->u64{
    RTC_MANGE.lock().await.as_mut().unwrap().current_time_us() / 1000
}
pub async fn get_sleep_ms()->u64{
    get_rtc_ms().await - unsafe { *core::ptr::addr_of!(WHEN_SLEEP_RTC_MS) }
}

pub async fn add_rtcio(rtcpin: &'static mut dyn RtcPinWithResistors, wakeup_level: WakeupLevel){
    unsafe {
        (*core::ptr::addr_of_mut!(WAKEUP_PINS)).push((rtcpin, wakeup_level));
    }
}
