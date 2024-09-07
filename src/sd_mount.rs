use alloc::string::ToString;
use core::str::FromStr;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{Directory, Error, File, SdCard, Volume, VolumeManager};
use embassy_time::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;
use esp_hal::gpio::GpioPin;
use esp_println::println;
use heapless::{String, Vec};
use crate::make_static;
use crate::sd_mount::SdError::{OpenBooksError, OpenRootError, OpenVolumeError, RootAlreadyOpen};

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
pub type SdCsPin = Output<'static,GpioPin<0>> ;
pub type ActualSdCard = SdCard<&'static mut CriticalSectionDevice<'static,Spi<'static,SPI2, FullDuplexMode>, SdCsPin, Delay>, Delay>;
pub type ActualVolumeManager = VolumeManager<ActualSdCard, TimeSource>;
pub type ActualVolume<'a> = Volume<'a,ActualSdCard, TimeSource,4,4,1>;
pub type ActualDirectory<'a> = Directory<'a,ActualSdCard, TimeSource,4,4,1>;


//pub static SDCARD_VOLUME_MGR_REF:Mutex<CriticalSectionRawMutex,Option<ActualVolumeManager<'static, SdCsPin>>> = Mutex::new(None);

pub static  SD_MOUNT:Mutex<CriticalSectionRawMutex,Option<SdMount>> = Mutex::new(None);

#[derive(Debug)]
pub enum SdError{
    OpenVolumeError,
    OpenRootError,
    OpenBooksError,
    RootAlreadyOpen
}

pub struct SdMount{
    pub volume_manager: ActualVolumeManager,

}
/*pub struct MyStruct{
    pub volume_manager: ActualVolumeManager<'a>,
}

impl MyStruct{
    pub fn get_open_volume<'a>(&'a mut self)->&'a mut String<10>{
        return self.open_volume.as_mut().unwrap();
    }
}*/

impl SdMount{
    pub fn new(volume_manager: ActualVolumeManager)->Self{
        Self{ volume_manager}
    }


    pub  fn get_books(books_dir:&mut ActualDirectory)->Result<Vec<String<20>,40>,SdError>
    {
        let mut books:Vec<String<20>,40> = Vec::new();


        books_dir.iterate_dir(|directory|{
                   if directory.name.extension() == b"TXT"{
                       let name = String::from_utf8(Vec::try_from(directory.name.base_name()).unwrap()).unwrap();
                       books.push(name);
                   }
               }).unwrap();

                Ok(books)

    }



}
