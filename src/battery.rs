use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use esp_hal::analog::adc::AdcPin;
use esp_hal::ram;
use esp_println::println;
use crate::event::ADC_PER;
use micromath::F32Ext;
pub struct Battery{
    pub voltage:u32,
    pub percent:u32,
}

impl  Battery{
    pub fn new()->Battery{
        Self{
            voltage: 0,
            percent: 0,
        }
    }
}

pub static BATTERY:Mutex<CriticalSectionRawMutex,Option<Battery>> = Mutex::new(None);
pub static ADC_PIN:Mutex<CriticalSectionRawMutex,Option<AdcPin<esp_hal::peripherals::GPIO4<'static>,esp_hal::peripherals::ADC1,esp_hal::analog::adc::AdcCalCurve<esp_hal::peripherals::ADC1>>>> = Mutex::new(None);

/// 最近一次电量百分比，RTC 内存保存，sleep_renderer 可同步读取
#[ram(unstable(rtc_fast))]
static mut LAST_BATTERY_PERCENT: u32 = 0;

#[embassy_executor::task]
pub async fn test_bat_adc() {
    const V_MAX: u32 = 4100;
    const V_MIN: u32 = 3100;

    loop {
        if let Some(v) = BATTERY.lock().await.as_mut() {
            if let Some(pin) = ADC_PIN.lock().await.as_mut() {
                if let Some(adc) = ADC_PER.lock().await.as_mut() {
                    loop {
                        match adc.read_oneshot(pin) {
                            Ok(adc_value) => {
                                let voltage_mv = adc_value as f32 * 2.0;
                                v.voltage = voltage_mv as u32;

                                let percent = if voltage_mv > V_MAX as f32 {
                                    100
                                } else if voltage_mv < V_MIN as f32 {
                                    0
                                } else {
                                    let normalized_voltage = (voltage_mv - V_MIN as f32) / (V_MAX - V_MIN) as f32;
                                    let curved_percent = (normalized_voltage * normalized_voltage * 100.0).round();
                                    curved_percent.min(100.0).max(0.0) as u32
                                };

                                v.percent = percent;
                                unsafe { *core::ptr::addr_of_mut!(LAST_BATTERY_PERCENT) = percent; }

                                println!("ADC原始值: {}", adc_value);
                                println!("电压: {} mV", v.voltage);
                                println!("电量: {}%", v.percent);

                                if v.percent < 20 {
                                    println!("警告：电量低 ({}%)", v.percent);
                                }
                                break;
                            }
                            Err(e) => {
                                println!("ADC错误: {:?}", e);
                            }
                        }
                    }
                }
            }
        }

        Timer::after_secs(60).await;
    }
}
