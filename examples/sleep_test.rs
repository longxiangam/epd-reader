//! v22 纯净深睡测试：只做 深睡 + IO2 低电平唤醒 + 30 秒定时兜底，无任何业务逻辑。
//! 样板与固件 main.rs 完全一致（esp_rtos 入口 + dram2 堆）。
//!
//! 构建：cargo build --release --example sleep_test
//! 烧录：espflash flash target/riscv32imc-unknown-none-elf/release/examples/sleep_test
//!
//! 现象判读：
//! - 5 秒倒计时后睡下，串口断开；30 秒整点重启（wake_cause=Timer）→ 深睡正常
//! - 按住 IO2 键拉低 → 立刻重启（wake_cause=Gpio）→ 按键唤醒正常
//! - 打印狂刷、睡不下 → 秒醒复现，看 wake_cause 是 Undefined 还是 Gpio

#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![allow(unused_must_use)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeupLevel};
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

// ── regi2c SAR ADC 快照/恢复（验证：ADC 的模拟 I2C 配置残留是否为秒醒毒源）──
const SAR_I2C_BLOCK: u8 = 0x69; // REGI2C_SAR_I2C
const SAR_I2C_HOST: u8 = 0;
const SAR_REGS: [u8; 4] = [0, 1, 2, 7]; // REG0/1: initial code, REG2: dref, REG7: encal

static mut REGI2C_SNAPSHOT: [u8; 4] = [0; 4];

unsafe extern "C" {
    fn esp_rom_regi2c_read(block: u8, block_hostid: u8, reg_add: u8) -> u8;
    fn rom_i2c_writeReg(block: u8, block_hostid: u8, reg_add: u8, indata: u8);
}

fn regi2c_capture() {
    unsafe {
        let mut snap = [0u8; 4];
        for (i, &reg) in SAR_REGS.iter().enumerate() {
            snap[i] = esp_rom_regi2c_read(SAR_I2C_BLOCK, SAR_I2C_HOST, reg);
        }
        core::ptr::addr_of_mut!(REGI2C_SNAPSHOT).write(snap);
        println!("regi2c snapshot {:02x?}", snap);
    }
}

fn regi2c_restore() {
    unsafe {
        let snap = core::ptr::addr_of!(REGI2C_SNAPSHOT).read();
        for (i, &reg) in SAR_REGS.iter().enumerate() {
            rom_i2c_writeReg(SAR_I2C_BLOCK, SAR_I2C_HOST, reg, snap[i]);
        }
        println!("regi2c restored {:02x?}", snap);
    }
}

// ── APB_SARADC / I2C_ANA_MST 整块快照 → ADC 操作后比对差异并恢复 ──
const APB_SARADC: u32 = 0x6000_E000;
const I2C_ANA_MST: u32 = 0x6004_8C00;
static mut SNAP_SAR: [u32; 26] = [0; 26]; // 0x00..0x64
static mut SNAP_ANA: [u32; 3] = [0; 3]; // 0x00..0x08

unsafe fn blocks_snapshot() {
    for i in 0..26 {
        SNAP_SAR[i] = ((APB_SARADC + 4 * i as u32) as *const u32).read_volatile();
    }
    for i in 0..3 {
        SNAP_ANA[i] = ((I2C_ANA_MST + 4 * i as u32) as *const u32).read_volatile();
    }
}

/// 打印 ADC 造成的寄存器差异，并恢复为 ADC 操作前的状态
unsafe fn blocks_diff_restore() {
    for i in 0..26 {
        let addr = (APB_SARADC + 4 * i as u32) as *mut u32;
        let now = addr.read_volatile();
        if now != SNAP_SAR[i] {
            println!("SARADC+{:#04x}: {:08x} -> {:08x}", i * 4, SNAP_SAR[i], now);
            addr.write_volatile(SNAP_SAR[i]);
        }
    }
    for i in 0..3 {
        let addr = (I2C_ANA_MST + 4 * i as u32) as *mut u32;
        let now = addr.read_volatile();
        if now != SNAP_ANA[i] {
            println!("ANAMST+{:#04x}: {:08x} -> {:08x}", i * 4, SNAP_ANA[i], now);
            addr.write_volatile(SNAP_ANA[i]);
        }
    }
}

/// 深睡前静默 SAR ADC：清掉单次采样的 START/通道配置/中断标志，
/// 并把 regi2c 的 SAR 校准配置归零。残留的采样状态会让模拟 SAR ADC
/// 在深睡中继续动作（GPIO2 通道的采样开关扰动 pad）→ 低电平唤醒误触发。
unsafe fn adc_silence() {
    let base = APB_SARADC as *mut u32;
    base.write_volatile(0); // CTRL: 清 START 等
    base.add(0x20 / 4).write_volatile(0); // ONETIME_SAMPLE: 清通道/衰减/START
    base.add(0x4C / 4).write_volatile(!0u32); // INT_CLR: 清全部中断标志
    for &reg in SAR_REGS.iter() {
        rom_i2c_writeReg(SAR_I2C_BLOCK, SAR_I2C_HOST, reg, 0); // regi2c SAR 归零
    }
    println!("adc silenced (ctrl/onetime/int/regi2c cleared)");
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

    println!("=== v22 纯净睡眠测试 ===");
    let config = HalConfig::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = hal_init(config);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // 唤醒重启后 USB 重新枚举需要时间：先等 3 秒，保证下面的打印能被串口终端收到
    let delay = Delay::new();
    delay.delay_millis(3000);

    let reason =
        esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu).unwrap_or(SocResetReason::ChipPowerOn);
    println!("reset={:?} wake_cause={:?}", reason, wakeup_cause());

    let mut rtc = Rtc::new(peripherals.LPWR);
    let delay = Delay::new();
    // IO1：SD+EINK 共用电源（低=开）
    let mut pwr = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());
    let _ = &mut pwr;

    // ── ADC 毒源验证：配置 + 采样一次（v22 固件同款操作）──
    use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
    unsafe { blocks_snapshot() }; // ADC 前快照外设块
    regi2c_capture(); // ADC 初始化前快照 regi2c 上电默认值
    let mut adc1_config = AdcConfig::new();
    let mut adc1_pin =
        adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
            unsafe { peripherals.GPIO2.clone_unchecked() },
            Attenuation::_11dB,
        );
    let mut adc1 = Adc::new(peripherals.ADC1, adc1_config);
    for _ in 0..5 {
        match adc1.read_oneshot(&mut adc1_pin) {
            Ok(v) => {
                println!("ADC GPIO2 = {}", v);
                break;
            }
            Err(_) => delay.delay_millis(2),
        }
    }
    regi2c_restore(); // 恢复 regi2c 快照（观察用）
    unsafe { blocks_diff_restore() }; // 恢复并打印差异（观察用）
    unsafe { adc_silence() }; // 深睡前静默 SAR ADC

    // IO2 唤醒脚：**必须在 ADC 初始化之后**配置数字输入 + 上拉——
    // set_analog 会关掉 fun_ie/fun_wpu（输入断开 → 深睡比较器悬空判低 → 秒醒），
    // 在 ADC 之后创建 Input 重新打开这两位。
    let mut wake_input = Input::new(
        unsafe { peripherals.GPIO2.clone_unchecked() },
        InputConfig::default().with_pull(Pull::Up),
    );
    let _ = &mut wake_input;

    for i in (1..=5).rev() {
        println!("深睡倒计时 {}s", i);
        delay.delay_millis(1000);
    }
    let _ = &mut wake_input; // 输入配置保持即可，深睡唤醒不依赖它

    // 30 秒定时兜底：若深睡正常，30 秒整点会以 Timer 原因重启
    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(30));
    let wakeup_pins: &mut [(&mut dyn esp_hal::gpio::RtcPinWithResistors, WakeupLevel)] =
        &mut [(&mut peripherals.GPIO2, WakeupLevel::Low)];
    let rtcio = RtcioWakeupSource::new(wakeup_pins);
    // 进深睡前再打一次复位/唤醒原因（串口重连错过开机打印时也能收到）
    let reason =
        esp_hal::rtc_cntl::reset_reason(Cpu::ProCpu).unwrap_or(SocResetReason::ChipPowerOn);
    println!(
        "before sleep: reset={:?} wake_cause={:?}",
        reason,
        wakeup_cause()
    );
    // 关键修复：标记 GPIO2 进 DIG_PAD_HOLD（bit2），深睡的 isolate_digital_gpio
    // 会跳过该脚，其数字输入 + IO_MUX 上拉穿越深睡保留。否则上拉被剥离、
    // pad 悬空，唤醒比较器判低 → 秒醒。（同 idf 对唤醒脚 gpio_hold_en 的效果）
    esp_hal::peripherals::LPWR::regs()
        .dig_pad_hold()
        .modify(|r, w| unsafe { w.bits(r.bits() | (1 << 2)) });
    println!("entering deep sleep (IO2 low to wake, 30s timer fallback)");
    delay.delay_millis(100);
    rtc.sleep_deep(&[&timer, &rtcio]);
}
