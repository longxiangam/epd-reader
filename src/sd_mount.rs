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
use heapless::String;
use crate::make_static;
use crate::sd_mount::SdError::{OpenRootError, OpenVolumeError, RootAlreadyOpen};

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
enum SdError{
    OpenVolumeError,
    OpenRootError,
    RootAlreadyOpen
}

pub struct SdMount<'a>{
    pub volume_manager: ActualVolumeManager,
    pub open_volume:Option<ActualVolume<'a>>,
    pub open_root:Option<ActualDirectory<'a>>
    //book_dir:Option<>

}
/*pub struct MyStruct{
    pub volume_manager: ActualVolumeManager<'a>,
}

impl MyStruct{
    pub fn get_open_volume<'a>(&'a mut self)->&'a mut String<10>{
        return self.open_volume.as_mut().unwrap();
    }
}*/

impl <'a> SdMount<'a>{
    pub fn new(volume_manager: ActualVolumeManager)->Self{
        Self{ volume_manager, open_volume: None, open_root: None }
    }

    pub async fn open_root<'b:'a>(&'b mut self)
                           ->Result<&'b ActualDirectory<'a> , SdError>
    {

        if let None = self.open_volume {
            let mut volume0 = self.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0));
            match volume0 {
                Ok(mut v) => {
                    self.open_volume = Some(v);
                }
                Err(e)=>{
                    return Err(OpenVolumeError);
                }
            }
        }

        if let None = self.open_root {
            let root_result = self.open_volume.as_mut().unwrap().open_root_dir();
            match root_result {
                Ok(root)=>{
                    self.open_root = Some(root);

                    return  Ok(self.open_root.as_mut().unwrap());
                }
                Err(e) => {
                    return Err(OpenRootError);
                },
            }
        }else{
            return Ok(self.open_root.as_mut().unwrap())
        }
    }

    pub  fn get_open_root(& mut self) ->& mut ActualDirectory<'a>
    {
        self.open_root.as_mut().unwrap()
    }



}
