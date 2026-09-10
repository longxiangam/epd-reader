//! 最小深睡测试：rtc_fast 计数器 + IO2 低电平唤醒 + 30 秒定时兜底。
//! 无 ADC / 无 I2C / 无 RTOS / 无任何业务——专测 rtc_fast 跨深睡保留。
//!
//! 构建：cargo build --release --example sleep_test
//! 烧录：espflash flash target/riscv32imc-unknown-none-elf/release/examples/sleep_test
//!
//! 判读（每次开机记三行）：
//! - reset=CoreDeepSleep + wake_cause=Timer/ Gpio + fast_seq 递增 → rtc_fast 保留 ✓
//! - reset=ChipPowerOn + wake_cause=Undefined + fast_seq=1 → 电源复位，rtc_fast 丢失

#![no_std]
#![no_main]

use esp_hal::delay::Delay;
use esp_hal::ram;
use esp_hal::gpio::{
    Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPin, RtcPinWithResistors,
};
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeupLevel};
use esp_hal::rtc_cntl::{wakeup_cause, Rtc, SocResetReason};
use esp_hal::system::Cpu;
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

/// rtc_fast 计数器：跨深睡唤醒保留则递增；归 1 = rtc_fast 丢失
#[ram(unstable(rtc_fast, persistent))]
static mut FAST_SEQ: u32 = 0;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

#[esp_hal::main]
fn main() -> ! {
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(DRAM2_HEAP) as *mut u8;
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            heap_ptr,
            65280,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();
    delay.delay_millis(3000); // 等串口重连

    let reason =
        esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu).unwrap_or(SocResetReason::ChipPowerOn);
    let mut rtc = Rtc::new(peripherals.LPWR);
    println!("=== 最小深睡测试 ===");
    println!("reset={:?} wake_cause={:?}", reason, wakeup_cause());

    unsafe {
        let fp = core::ptr::addr_of_mut!(FAST_SEQ);
        *fp = fp.read_volatile().wrapping_add(1);
    }
    let fast_seq = unsafe { core::ptr::addr_of!(FAST_SEQ).read_volatile() };
    println!("fast_seq={}", fast_seq);

    // IO1：电源（低=开）
    let mut pwr = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());
    let _ = &mut pwr;

    // IO2：唤醒脚（输入 + 上拉）
    let mut wake = Input::new(
        unsafe { peripherals.GPIO2.clone_unchecked() },
        InputConfig::default().with_pull(Pull::Up),
    );
    let _ = &mut wake;

    for i in (1..=5).rev() {
        println!("deep sleep in {}s (IO2 low to wake / 30s timer)", i);
        delay.delay_millis(1000);
    }

    let timer =
        esp_hal::rtc_cntl::sleep::TimerWakeupSource::new(core::time::Duration::from_secs(30));
    let pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] =
        &mut [(&mut unsafe { peripherals.GPIO2.clone_unchecked() }, WakeupLevel::Low)];
    let rtcio = RtcioWakeupSource::new(pins);
    println!("entering deep sleep");
    delay.delay_millis(100);
    rtc.sleep_deep(&[&timer, &rtcio]);
}
