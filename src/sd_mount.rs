use alloc::string::ToString;
use core::str::FromStr;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{Directory, Error, File, LfnBuffer, SdCard, Volume, VolumeManager};
use embassy_time::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;
use esp_hal::gpio::GpioPin;
use esp_println::println;
use heapless::{String, Vec};
use log::info;
use crate::make_static;
use crate::sd_mount::SdError::{OpenBooksError, OpenRootError, OpenVolumeError, RootAlreadyOpen, FileNotFound};

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
pub type SdCsPin = Output<'static,GpioPin<5>> ;
pub type ActualSdCard = SdCard<&'static mut CriticalSectionDevice<'static,Spi<'static,SPI2, FullDuplexMode>, SdCsPin, Delay>, Delay>;
pub type ActualVolumeManager = VolumeManager<ActualSdCard, TimeSource>;
pub type ActualVolume<'a> = Volume<'a,ActualSdCard, TimeSource,4,4,1>;
pub type ActualDirectory<'a> = Directory<'a,ActualSdCard, TimeSource,4,4,1>;
pub type ActualFile<'a> = File<'a,ActualSdCard, TimeSource,4,4,1>;

//pub static SDCARD_VOLUME_MGR_REF:Mutex<CriticalSectionRawMutex,Option<ActualVolumeManager<'static, SdCsPin>>> = Mutex::new(None);

pub static  SD_MOUNT:Mutex<CriticalSectionRawMutex,Option<SdMount>> = Mutex::new(None);

#[derive(Debug)]
pub enum SdError{
    OpenVolumeError,
    OpenRootError,
    OpenBooksError,
    RootAlreadyOpen,
    FileNotFound,
}

pub struct SdMount{
    pub volume_manager: ActualVolumeManager,

}

impl SdMount{
    pub fn new(volume_manager: ActualVolumeManager)->Self{
        Self{ volume_manager}
    }


    pub  fn get_books(books_dir:&mut ActualDirectory)->Result<Vec<String<50>,40>,SdError>
    {
        let mut books:Vec<String<50>,40> = Vec::new();

        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        books_dir.iterate_dir_lfn(&mut lfn_buffer,|directory, lfn|{
          /*  if directory.name.extension() == b"TXT"{
                let name = String::from_utf8(Vec::try_from(directory.name.base_name()).unwrap()).unwrap();
                books.push(name);
            }*/

            println!(
                "{:12} {:9} {} {} {:08X?} {:5?}",
                directory.name,
                directory.size,
                directory.ctime,
                directory.mtime,
                directory.cluster,
                directory.attributes,
            );
            if let Some(lfn) = lfn {
                let lnf_name_str: Result<heapless::String<50>, ()> = String::from_str(lfn);
                match lnf_name_str.as_ref() {
                    Ok(v) => {
                        let temp = v.to_uppercase();
                        if temp.ends_with(".TXT") {
                            let name_without_ext = temp.trim_end_matches(".TXT");
                            let _ = books.push(String::from_str(name_without_ext).unwrap());
                        }
                    }
                    Err(e) => {
                        println!("{:?}",e)
                    }
                }
                
               
            } else {
                if directory.name.extension() == b"TXT"{
                    let name = String::from_utf8(Vec::try_from(directory.name.base_name()).unwrap()).unwrap();
                   let _ = books.push(name);
                } 
            }

        }).unwrap();

        Ok(books)

    }

    pub fn find_entry_by_name<'a>(books_dir: &'a mut ActualDirectory, file_name: &str) -> Option<embedded_sdmmc::DirEntry> {
        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        let mut target_entry = None;

        let _ = books_dir.iterate_dir_lfn(&mut lfn_buffer, |directory, lfn| {
            if let Some(lfn_name) = lfn {
                println!("lfn_name:{}",lfn_name);
                println!("file_name:{}",file_name);
                if lfn_name.to_uppercase() == file_name.to_uppercase() {
                    target_entry = Some(directory.clone());
                }
            } else {
                let full_name = directory.name.to_string();
                if full_name == file_name.to_uppercase() {
                    target_entry = Some(directory.clone());
                }
            }
        });

        target_entry
    }

    pub fn open_file_by_name<'a>(books_dir: &'a mut ActualDirectory, file_name: &str, mode: embedded_sdmmc::Mode) -> Result<ActualFile<'a>, SdError> {
        if let Some(entry) = Self::find_entry_by_name(books_dir, file_name) {
            println!("entry_name:{}",entry.name);
            if let Ok(temp) =  books_dir.open_file_in_dir(entry.name,mode) {
                return Ok(temp);
            }else{
                Err(SdError::FileNotFound)
            }
        } else {
            Err(SdError::FileNotFound)
        }
    }
}
