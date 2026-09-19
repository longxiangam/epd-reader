//! v22 板级辅助：I2C 构造、MISO 上拉、传感器任务、深睡诊断。
//! 按键检测在 event.rs（master 方案：边沿检测 + ADC 分键），
//! 睡眠流程在 sleep.rs（master 方案：断轨 + sleep_deep）。

use esp_println::println;

/// v22 诊断：每次启动自增——跨深睡唤醒保持递增 = rtc_fast 保留正常；
/// 归 1 = rtc_fast 在深睡中丢失。
#[esp_hal::ram(unstable(rtc_fast))]
pub static mut RTC_FAST_BOOT_SEQ: u32 = 0;

/// v22：构造 I2C 控制器（GPIO9=SDA，GPIO21=SCL，挂 PCF8563 + AHT20）。
/// 需在任何外设被 move 之前调用。
pub fn init_i2c(
    p: &mut esp_hal::peripherals::Peripherals,
) -> Option<esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>> {
    Some(
        esp_hal::i2c::master::I2c::new(
            unsafe { p.I2C0.clone_unchecked() },
            esp_hal::i2c::master::Config::default(),
        )
        .expect("i2c init")
        .with_sda(unsafe { p.GPIO9.clone_unchecked() })
        .with_scl(unsafe { p.GPIO21.clone_unchecked() }),
    )
}

/// v22：MISO(GPIO10) 内部上拉——SD 卡未初始化前 DO 高阻，无上拉时 CMD0
/// 应答读不可靠。需在 SPI 占用 GPIO10 之前调用。
pub fn sd_miso_pullup(p: &mut esp_hal::peripherals::Peripherals) {
    let miso_pu = esp_hal::gpio::Input::new(
        unsafe { p.GPIO10.clone_unchecked() },
        esp_hal::gpio::InputConfig::default().with_pull(esp_hal::gpio::Pull::Up),
    );
    core::mem::forget(miso_pu); // 配置保持到整个运行期
}

/// AHT20 温湿度采样任务。
pub fn spawn_sensors(sp: &embassy_executor::Spawner) {
    sp.spawn(crate::aht20::sample_task().unwrap());
}

/// 诊断打印（rtc_fast 状态）
pub fn print_boot_diag() {
    let (when_ts, clk_sync) = crate::worldtime::diag_rtc_fast();
    unsafe {
        let p = core::ptr::addr_of_mut!(RTC_FAST_BOOT_SEQ);
        *p = p.read_volatile().wrapping_add(1);
        println!(
            "[diag] rtc_fast boot_seq={} when_sleep_ts={} clk_sync={}",
            p.read_volatile(),
            when_ts,
            clk_sync
        );
    }
}
