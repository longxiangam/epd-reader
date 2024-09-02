use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{File, SdCard, Volume, VolumeManager};
use embassy_time::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;
use esp_hal::gpio::GpioPin;

pub struct TimeSource;


impl embedded_sdmmc::TimeSource for TimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub type ActualSdCard<'a,CS: esp_hal::gpio::OutputPin> = SdCard<&'a mut CriticalSectionDevice<'a,Spi<'a,SPI2, FullDuplexMode>, Output<'a,CS>, Delay>, Delay>;
pub type ActualVolumeManager<'a,CS: esp_hal::gpio::OutputPin> = VolumeManager<ActualSdCard<'a,CS>, TimeSource>;
pub static SDCARD_REF:Mutex<CriticalSectionRawMutex,Option<ActualVolumeManager<'static, GpioPin<0>>>> = Mutex::new(None);

