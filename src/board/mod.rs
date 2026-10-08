//! 板级定义：`epd*` feature 选屏驱动，`board_*` feature 选板硬件，
//! 一个设备 = epd + board 组合，编译期固定全部 IO 分配。
//!
//! 每块板的引脚与外设定义在独立文件：
//! - [`v1`]：旧板（引脚沿用 master 分支，可配 2.9 / 2.7 / 4.2 三种屏）
//! - [`v2`]：v22 新板（4.2 寸屏）
//!
//! 本文件只放屏型/板型守卫、两代板相同的 EPD/SPI/SD 常量与诊断。
//!
//! 编译矩阵：
//! - v2 新板：    `--features epd4in2,board_v2`
//! - v1 旧板 4.2：`--features epd4in2,board_v1`
//! - v1 旧板 2.9：`--features epd2in9,board_v1`
//! - v1 旧板 2.7：`--features epd2in7,board_v1`

use esp_println::println;

// ── 屏型守卫：三选一 ──
#[cfg(all(feature = "epd2in9", feature = "epd2in7"))]
compile_error!("屏型 feature 只能选一个：epd2in9 / epd2in7 / epd4in2");
#[cfg(all(feature = "epd2in9", feature = "epd4in2"))]
compile_error!("屏型 feature 只能选一个：epd2in9 / epd2in7 / epd4in2");
#[cfg(all(feature = "epd2in7", feature = "epd4in2"))]
compile_error!("屏型 feature 只能选一个：epd2in9 / epd2in7 / epd4in2");
#[cfg(not(any(feature = "epd2in9", feature = "epd2in7", feature = "epd4in2")))]
compile_error!("必须选择屏型 feature：epd2in9 / epd2in7 / epd4in2");

// ── 板型守卫：二选一 ──
#[cfg(all(feature = "board_v1", feature = "board_v2"))]
compile_error!("板型 feature 只能选一个：board_v1 / board_v2");
#[cfg(not(any(feature = "board_v1", feature = "board_v2")))]
compile_error!("必须选择板型 feature：board_v1 / board_v2");
// v2 新板只出 4.2 寸屏
#[cfg(all(feature = "board_v2", any(feature = "epd2in9", feature = "epd2in7")))]
compile_error!("board_v2 板只能配 epd4in2 屏");

#[cfg(feature = "board_v1")]
pub mod v1;
#[cfg(feature = "board_v2")]
pub mod v2;

// 统一再导出：调用方一律 `board::xxx`，不感知具体板文件
#[cfg(feature = "board_v1")]
pub use v1::*;
#[cfg(feature = "board_v2")]
pub use v2::*;

// ── EPD / SPI / SD（两代板相同）──
pub const EPD_BUSY_GPIO: u8 = 6;
pub const EPD_RST_GPIO: u8 = 7;
pub const EPD_DC_GPIO: u8 = 20;
pub const EPD_CS_GPIO: u8 = 3;
pub const SPI_SCLK_GPIO: u8 = 8;
pub const SPI_MOSI_GPIO: u8 = 0;
pub const SPI_MISO_GPIO: u8 = 10;
pub const SD_CS_GPIO: u8 = 5;

/// MISO(GPIO10) 内部上拉——SD 卡未初始化前 DO 高阻，无上拉时 CMD0 应答读不可靠。
/// 需在 SPI 占用 GPIO10 之前配置（两代板相同）。
pub fn sd_miso_pullup(p: &mut esp_hal::peripherals::Peripherals) {
    let miso_pu = esp_hal::gpio::Input::new(
        unsafe { p.GPIO10.clone_unchecked() },
        esp_hal::gpio::InputConfig::default().with_pull(esp_hal::gpio::Pull::Up),
    );
    core::mem::forget(miso_pu); // 配置保持到整个运行期
}

/// 板载传感器任务（v2：AHT20 温湿度；v1 无）
pub fn spawn_sensors(sp: &embassy_executor::Spawner) {
    #[cfg(feature = "board_v2")]
    sp.spawn(crate::aht20::sample_task().unwrap());
    #[cfg(feature = "board_v1")]
    let _ = sp;
}

/// v1/v2 诊断：每次启动自增——跨深睡唤醒保持递增 = rtc_fast 保留正常；
/// 归 1 = rtc_fast 在深睡中丢失。
#[esp_hal::ram(unstable(rtc_fast))]
pub static mut RTC_FAST_BOOT_SEQ: u32 = 0;

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
