//! v1 旧板定义（引脚沿用 master 分支）：可配 2.9 / 2.7 / 4.2 三种屏。
//!
//! - 无 I2C 外设（无 AHT20 / PCF8563）
//! - 双电源轨：EINK=GPIO21 / SD=GPIO1（高=断，初始 High），深睡各自断轨
//! - 按键：数字直连键 GPIO9（键1）+ 三键梯 GPIO2（键2/3 走 ADC，兼深睡唤醒）
//! - 电池分压 ADC：GPIO4

/// 三键梯（ADC 分压区分键号）+ 深睡唤醒脚
pub const KEY_LADDER_GPIO: u8 = 2;
/// 数字直连键（键1）
pub const KEY_DIRECT_GPIO: u8 = 9;
/// 电池分压 ADC
pub const BAT_ADC_GPIO: u8 = 4;
/// 无 PCF8563（定时唤醒走 ESP RTC；时钟回落 ESP RTC）
pub const HAS_PCF: bool = false;
/// 无 AHT20
pub const HAS_AHT20: bool = false;

pub type KeyLadderGpio = esp_hal::peripherals::GPIO2<'static>;
pub type BatAdcGpio = esp_hal::peripherals::GPIO4<'static>;

use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};

/// 深睡 pad hold：给 GPIO1(SD 轨) 打 hold（master 行为：深睡中保持轨状态）。
pub fn init_pad_hold(p: &mut esp_hal::peripherals::Peripherals) {
    use esp_hal::gpio::RtcPin;
    unsafe { p.GPIO1.clone_unchecked() }.rtcio_pad_hold(true);
}

/// 旧板无 I2C 外设
pub fn init_i2c(
    _p: &mut esp_hal::peripherals::Peripherals,
) -> Option<esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>> {
    None
}

/// 电源轨：EINK=GPIO21 / SD=GPIO1（高=断）。填 sleep 两槽，睡眠各自置高断轨；
/// 初始化后立即拉低上电（master 行为）。
pub async fn init_power_rails(p: &mut esp_hal::peripherals::Peripherals) {
    let mut eink_pwr_ctrl =
        Output::new(unsafe { p.GPIO21.clone_unchecked() }, Level::High, OutputConfig::default());
    let mut sd_pwr_ctrl =
        Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::High, OutputConfig::default());
    // 上电：两轨拉低
    eink_pwr_ctrl.set_low();
    sd_pwr_ctrl.set_low();
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}

// 以下引脚在 main 里 EPD/SPI 外设部分 move 之后再取——不能借 &mut Peripherals，
// 统一走 steal()（本工程 clone_unchecked 同款惯例）。

/// 三键梯 GPIO
pub fn key_ladder_pin() -> KeyLadderGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO2.clone_unchecked() }
}

/// 数字直连键（键1）
pub fn key_direct_pin() -> esp_hal::peripherals::GPIO9<'static> {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO9.clone_unchecked() }
}

/// 电池分压 GPIO
pub fn bat_pin() -> BatAdcGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO4.clone_unchecked() }
}
