//! 最小深睡 + ADC 按钮测试（GPIO4 三键梯）：
//! 1. 上电 → 打印复位/唤醒/fast_seq
//! 2. 按钮交互模式：按按键打印 ADC 原始值与毫伏（观察梯子分压点）
//! 3. 15 秒无按键 → 断轨深睡（30s 定时兜底，按钮可唤醒，fast_seq 验证 rtc_fast）
//!
//! 烧录：espflash flash target/riscv32imc-unknown-none-elf/release/examples/sleep_test

#![no_std]
#![no_main]

use esp_hal::analog::adc::{Adc, AdcConfig, AdcCalCurve, Attenuation};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPinWithResistors};
use esp_hal::ram;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeupLevel};
use esp_hal::rtc_cntl::{Rtc, SocResetReason, wakeup_cause};
use esp_hal::system::Cpu;
use esp_hal::time::{Duration, Instant};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

// 仅为了把 esp_alloc 的 #[global_allocator] 链入（依赖图中有代码需要 alloc）
extern crate esp_alloc;

/// rtc_fast 计数器：跨深睡唤醒保留则递增；归 1 = POR 丢失
#[ram(unstable(rtc_fast))]
static mut FAST_SEQ: u32 = 0;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

/// 按键 ADC（GPIO4 / ADC1）
struct KeyAdc {
    adc: Adc<'static, esp_hal::peripherals::ADC1<'static>, esp_hal::Blocking>,
    pin: esp_hal::analog::adc::AdcPin<
        esp_hal::peripherals::GPIO4<'static>,
        esp_hal::peripherals::ADC1<'static>,
        esp_hal::analog::adc::AdcCalCurve<esp_hal::peripherals::ADC1<'static>>,
    >,
}

impl KeyAdc {
    /// 多次采样取均值；WouldBlock 重试（esp-hal errata：偶发启动脉冲丢失）
    fn avg(&mut self, delay: &mut Delay, samples: usize) -> u16 {
        let mut sum = 0u32;
        let mut got = 0usize;
        let mut tries = 0usize;
        while got < samples && tries < 200 {
            tries += 1;
            match self.adc.read_oneshot(&mut self.pin) {
                Ok(v) => {
                    sum += v as u32;
                    got += 1;
                }
                Err(_) => delay.delay_millis(2),
            }
        }
        if got == 0 { 0 } else { (sum / got as u32) as u16 }
    }

    /// 中位数采样：抗按键过渡期/机械抖动的瞬态毛刺
    fn median(&mut self, delay: &mut Delay, samples: usize) -> u16 {
        let mut buf = [0u16; 15];
        let n = samples.min(buf.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.avg(delay, 1);
        }
        buf[..n].sort_unstable();
        buf[n / 2]
    }
}

fn to_mv(v: u16) -> u32 {
    (v as u32 * 3300) / 4095
}

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // IO1 立即驱低上轨（低=开）
    let mut pwr = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());

    // 按键脚：运行期数字输入 + 内部上拉
    let _wake = Input::new(
        unsafe { peripherals.GPIO4.clone_unchecked() },
        InputConfig::default().with_pull(Pull::Up),
    );

    // 按键 ADC（同一 pad）
    let mut adc1_config = AdcConfig::new();
    let key_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
        unsafe { peripherals.GPIO4.clone_unchecked() },
        Attenuation::_11dB,
    );
    let mut key_adc = KeyAdc {
        adc: Adc::new(peripherals.ADC1, adc1_config),
        pin: key_pin,
    };

    let mut delay = Delay::new();
    delay.delay_millis(3000); // 等串口重连

    let reason =
        esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu).unwrap_or(SocResetReason::ChipPowerOn);
    unsafe {
        let fp = core::ptr::addr_of_mut!(FAST_SEQ);
        *fp = fp.read_volatile().wrapping_add(1);
    }
    println!(
        "reset={:?} wake={:?} fast_seq={}",
        reason,
        wakeup_cause(),
        unsafe { core::ptr::addr_of!(FAST_SEQ).read_volatile() }
    );

    // ── 按钮交互模式：按下打印键值；15 秒无操作进入深睡 ──
    let idle = key_adc.avg(&mut delay, 20);
    println!("[button] idle={} (~{}mV)，按按键会打印键值", idle, to_mv(idle));
    println!("[button] 15 秒无操作 → 深睡（30s 定时兜底，按按钮唤醒）");

    let mut last_activity = Instant::now();
    loop {
        let v = key_adc.avg(&mut delay, 8);
        if (v as i32 - idle as i32).abs() > 300 {
            delay.delay_millis(30); // 等触点/过渡期稳定
            let pressed = key_adc.median(&mut delay, 15);
            println!(
                "[button] pressed adc={} (~{}mV)  idle={}",
                pressed,
                to_mv(pressed),
                idle
            );
            // 等松开
            loop {
                let r = key_adc.avg(&mut delay, 8);
                if (r as i32 - idle as i32).abs() < 150 {
                    break;
                }
                delay.delay_millis(10);
            }
            println!("[button] released");
            last_activity = Instant::now();
        }

        if last_activity.elapsed() > Duration::from_secs(15) {
            println!("[button] 15 秒无操作 → 深睡");
            break;
        }
        delay.delay_millis(10);
    }

    // ── 深睡（30s 定时兜底 + GPIO4 低电平唤醒）──
    pwr.set_high(); // 睡前断轨
    delay.delay_millis(50); // 等轨放电

    let pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] = &mut [(
        &mut unsafe { peripherals.GPIO4.clone_unchecked() },
        WakeupLevel::Low,
    )];
    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(30));
    let mut rtc = Rtc::new(peripherals.LPWR);
    rtc.sleep_deep(&[&timer, &RtcioWakeupSource::new(pins)]);
}
