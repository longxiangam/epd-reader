use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{File, SdCard, Volume, VolumeManager};
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;



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

type ActualSdCard<'a,CS: esp_hal::gpio::OutputPin> = SdCard<&'a mut CriticalSectionDevice<'a,Spi<'a,SPI2, FullDuplexMode>, Output<'a,CS>, Delay>, Delay>;



struct SdMount<'a,'b,CS: esp_hal::gpio::OutputPin>{
    volume_mgr:VolumeManager<ActualSdCard<'a,CS>,TimeSource>,
    volume0:Volume<'b,ActualSdCard<'a,CS>, TimeSource, 4, 4, 1>
}

impl <'a,'b, CS: esp_hal::gpio::OutputPin> SdMount<'a,'b,CS>{
    pub fn new(){

    }
    fn open_root(){

    }
}