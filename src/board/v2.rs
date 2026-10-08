//! v2 新板定义（即 v22）：4.2 寸屏。
//!
//! - I2C：GPIO9=SDA / GPIO21=SCL，挂 AHT20（0x38）+ PCF8563（0x51）
//! - 单电源轨：GPIO1（低=开），EINK/SD 两槽填同一轨——睡眠时统一置高断轨
//! - 按键：三键梯全在 GPIO4（ADC 分压，兼深睡唤醒，PCF INT 也拉低此脚）
//! - 电池分压 ADC：GPIO2

/// 三键梯（ADC 分压区分键号）+ 深睡唤醒脚
pub const KEY_LADDER_GPIO: u8 = 4;
/// 电池分压 ADC
pub const BAT_ADC_GPIO: u8 = 2;
/// 有 PCF8563（定时唤醒/时钟源）
pub const HAS_PCF: bool = true;
/// 有 AHT20（室内温湿度）
pub const HAS_AHT20: bool = true;

pub type KeyLadderGpio = esp_hal::peripherals::GPIO4<'static>;
pub type BatAdcGpio = esp_hal::peripherals::GPIO2<'static>;

use esp_hal::gpio::{Level, Output, OutputConfig};

/// 深睡 pad hold：释放上次深睡布下的 hold（RTC 域跨深睡保留——不释放的话
/// GPIO1 的驱低上轨写不进去 → 轨断电 → 显示/SD 全灭）。
pub fn init_pad_hold(_p: &mut esp_hal::peripherals::Peripherals) {
    esp_hal::peripherals::LPWR::regs()
        .pad_hold()
        .modify(|_, w| unsafe { w.bits(0) });
}

/// I2C 总线。需在任何外设被 move 之前调用。
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

/// 电源轨：单轨 GPIO1（低=开），EINK/SD 两槽填同一轨——睡眠时统一置高断轨，
/// 与旧板双电源脚等价。
pub async fn init_power_rails(p: &mut esp_hal::peripherals::Peripherals) {
    let mut eink_pwr_ctrl =
        Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    let mut sd_pwr_ctrl =
        Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}

// 以下引脚在 main 里 EPD/SPI 外设部分 move 之后再取——不能借 &mut Peripherals，
// 统一走 steal()（本工程 clone_unchecked 同款惯例）。

/// 三键梯 GPIO（ADC 分压 + 数字输入 + 深睡唤醒共用一 pad）
pub fn key_ladder_pin() -> KeyLadderGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO4.clone_unchecked() }
}

/// 电池分压 GPIO
pub fn bat_pin() -> BatAdcGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO2.clone_unchecked() }
}
