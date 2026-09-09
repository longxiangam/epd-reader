//! 对照版：只有 30 秒定时唤醒，**不挂 IO2 电平唤醒**。
//! - 30 秒整点重启（wake_cause=Timer）→ 深睡/定时唤醒正常
//! - 仍然秒醒 → 问题在深睡本身（esp-hal 配置/电源），与 IO 唤醒无关
//!
//! 构建：cargo build --release --example sleep_test_timer
//! 烧录：espflash flash target/riscv32imc-unknown-none-elf/release/examples/sleep_test_timer

#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![allow(unused_must_use)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::rtc_cntl::sleep::TimerWakeupSource;
use esp_hal::rtc_cntl::{wakeup_cause, Rtc, SocResetReason};
use esp_hal::system::Cpu;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{init as hal_init, interrupt::software::SoftwareInterruptControl, Config as HalConfig};
use esp_println::println;

#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {:?}", info);
    loop {}
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(DRAM2_HEAP) as *mut u8;
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            heap_ptr,
            65280,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }

    println!("=== v22 睡眠测试（仅定时唤醒，无 IO 唤醒）===");
    let config = HalConfig::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = hal_init(config);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let reason =
        esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu).unwrap_or(SocResetReason::ChipPowerOn);
    println!("reset={:?} wake_cause={:?}", reason, wakeup_cause());

    let mut rtc = Rtc::new(peripherals.LPWR);
    let delay = Delay::new();

    // IO1：SD+EINK 共用电源（低=开）
    let mut pwr = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());
    let _ = &mut pwr;

    for i in (1..=5).rev() {
        println!("深睡倒计时 {}s", i);
        delay.delay_millis(1000);
    }

    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(30));
    println!("entering deep sleep (30s timer only)");
    delay.delay_millis(100);
    rtc.sleep_deep(&[&timer]);
}
