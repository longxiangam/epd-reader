use core::cmp::max;
use embassy_executor::task;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use esp_println::println;
use esp_hal::analog::adc::{Adc, AdcCalBasic, AdcPin};
use esp_hal::gpio::{Analog, GpioPin};
use esp_hal::peripherals::ADC1;
use crate::event::ADC_PER;
use micromath::F32Ext;
type AdcCal = esp_hal::analog::adc::AdcCalCurve<ADC1>;
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
pub static ADC_PIN:Mutex<CriticalSectionRawMutex,Option<AdcPin<GpioPin<4>,ADC1,AdcCal>>> = Mutex::new(None);

#[task]
pub async fn test_bat_adc() {
    // 锂电池常量（4.2V最大电压）
    const V_MAX: u32 = 4100; // 4.2V（满电，毫伏）,有误差 -100
    const V_MIN: u32 = 3100; // 3.2V（截止电压，毫伏）


    loop {
        if let Some(v) = BATTERY.lock().await.as_mut() {
            if let Some(pin) = ADC_PIN.lock().await.as_mut() {
                if let Some(adc) = ADC_PER.lock().await.as_mut() {
                    loop {
                        match adc.read_oneshot(pin) {
                            Ok(adc_value) => {
                                // 将ADC读数转换为实际电压
                                let voltage_mv = adc_value as f32 * 2.0; // 假设1:2分压器
                                v.voltage = voltage_mv as u32;

                                // 使用简化的放电曲线计算电量百分比
                                let percent = if voltage_mv > V_MAX as f32 {
                                    100
                                } else if voltage_mv < V_MIN as f32 {
                                    0
                                } else {
                                    // 针对锂电池放电特性的非线性近似
                                    let normalized_voltage = (voltage_mv - V_MIN as f32) / (V_MAX - V_MIN) as f32;
                                    // 应用曲线以更好地匹配锂电池特性
                                    let curved_percent = (normalized_voltage * normalized_voltage * 100.0).round();
                                    curved_percent.min(100.0).max(0.0) as u32
                                };

                                v.percent = percent;

                                // 日志输出
                                println!("ADC原始值: {}", adc_value);
                                println!("电压: {} mV", v.voltage);
                                println!("电量: {}%", v.percent);

                                // 可选：低电量警告
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

        // 每60秒采样一次
        Timer::after_secs(60).await;
    }
}