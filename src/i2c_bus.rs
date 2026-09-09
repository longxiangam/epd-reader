//! v22：I2C 总线容器（GPIO9=SDA，GPIO20=SCL，100kHz —— PCF8563 安全上限）。
//! 挂载 AHT20（0x38）与 PCF8563（0x51），驱动经这里的助手函数访问总线。

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::i2c::master::I2c;

pub type I2cDev = I2c<'static, esp_hal::Blocking>;

pub static I2C: Mutex<CriticalSectionRawMutex, Option<I2cDev>> = Mutex::new(None);

pub async fn write(addr: u8, data: &[u8]) -> Result<(), ()> {
    use embedded_hal::i2c::I2c as _;
    let mut bus = I2C.lock().await;
    let dev = bus.as_mut().ok_or(())?;
    dev.write(addr, data).map_err(|_| ())
}

pub async fn write_read(addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ()> {
    use embedded_hal::i2c::I2c as _;
    let mut bus = I2C.lock().await;
    let dev = bus.as_mut().ok_or(())?;
    dev.write_read(addr, wr, rd).map_err(|_| ())
}
