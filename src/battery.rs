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

/// 11dB 衰减下 ADC 实际饱和电压（mV）：满电 4.2V 实测标定（raw=2937 ↔ 中点
/// 2100mV → 4095 满量程 ≈2929mV）。比规格"可用范围"推荐值 2500mV 高。
const PIN_FULL_SCALE_MV: f32 = 2929.0;
// 电池分压 ADC 引脚（板级定义见 board::BatAdcGpio：v22=GPIO2，旧板=GPIO4）
pub static ADC_PIN:Mutex<CriticalSectionRawMutex,Option<AdcPin<crate::board::BatAdcGpio,esp_hal::peripherals::ADC1,esp_hal::analog::adc::AdcCalCurve<esp_hal::peripherals::ADC1>>>> = Mutex::new(None);

/// 最近一次电量百分比，RTC 内存保存，sleep_renderer 可同步读取
#[ram(unstable(rtc_fast))]
static mut LAST_BATTERY_PERCENT: u32 = 0;

/// 同步读取最近一次电量百分比（供渲染路径用，无需 await）。
pub fn last_percent() -> u32 {
    unsafe { *core::ptr::addr_of!(LAST_BATTERY_PERCENT) }
}

#[embassy_executor::task]
pub async fn test_bat_adc() {
    const V_MAX: u32 = 4100;
    const V_MIN: u32 = 3100;

    loop {
        if let Some(v) = BATTERY.lock().await.as_mut() {
            if let Some(pin) = ADC_PIN.lock().await.as_mut() {
                if let Some(adc) = ADC_PER.lock().await.as_mut() {
                    let mut tries = 0u32;
                    loop {
                        tries += 1;
                        match adc.read_oneshot(pin) {
                            Ok(adc_value) => {
                                // raw(0..4095) → 分压中点电压 → ×2 还原电池电压
                                //（100k/100k 分压）。满量程用实测标定值：满电 4.2V
                                // 实测 raw=2937 ↔ 中点 2100mV → 饱和点 ≈2929mV——
                                // C3 在 11dB 衰减下实际饱和点比理论 2500mV（可用范围
                                // 推荐值）高。此前误写 raw×2（计数量当毫伏）恒 100%。
                                let voltage_mv =
                                    adc_value as f32 * (PIN_FULL_SCALE_MV * 2.0 / 4095.0);
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

                                // 诊断换算：pin = 分压中点电压估计（=电池电压/2），
                                // 供与万用表实测对比。
                                let pin_mv =
                                    (adc_value as f32 * (PIN_FULL_SCALE_MV / 4095.0)) as u32;
                                println!(
                                    "[bat] raw={} pin≈{}mV bat≈{}mV 电量{}%",
                                    adc_value, pin_mv, v.voltage, v.percent
                                );

                                if v.percent < 20 {
                                    println!("警告：电量低 ({}%)", v.percent);
                                }
                                break;
                            }
                            Err(e) => {
                                // ADC 偶发 Err 需重试；持续错误超过上限才放弃本轮（防死循环+持锁），保留上次电量。
                                println!("ADC错误: {:?}", e);
                                if tries >= 50 {
                                    break;
                                }
                                Timer::after_millis(2).await;
                            }
                        }
                    }
                }
            }
        }

        Timer::after_secs(60).await;
    }
}
