use core::mem::size_of;
use core::ptr;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_storage::{FlashStorage, FlashStorageError};
use embedded_storage::{ReadStorage, Storage};
use esp_println::println;
use crate::model::holiday::HolidayResponse;
use crate::model::seniverse::DailyResult;

pub fn write_flash(flash_addr:u32, bytes: &[u8]) -> Result<(), FlashStorageError> {
    let mut flash = FlashStorage::new();
    println!("write_flash:{}",flash_addr);
    let result = flash.write(flash_addr, bytes);

    match result {
        Ok(_) => { println!("save success");}
        Err(e) => {
            println!("save fail：{:?}",e);
        }
    }

    Ok(())

}
pub fn read_flash(flash_addr:u32, bytes: &mut [u8]) -> Result<(), FlashStorageError> {
    let mut flash = FlashStorage::new();
    //println!("read_flash:{}",flash_addr);
    let result = flash.read(flash_addr,bytes);
    match result {
        Ok(_) => {
            //println!("read success");
            Ok(())
        }
        Err(e) => {
            //println!("read fail：{:?}",e);
            Err(e)
        }
    }
}


fn serialize_storage<T>(storage: &T) -> [u8; size_of::<T>()] {
    unsafe { ptr::read(storage as *const _ as *const [u8; size_of::<T>()]) }
}

fn deserialize_storage<T>(data: &[u8]) -> T {
    unsafe { ptr::read(data.as_ptr() as *const T) }
}

// 分块写入大结构体，避免栈溢出
fn write_storage_chunked<T>(storage: &T, offset: u32) -> Result<(), FlashStorageError> {
    const CHUNK_SIZE: usize = 128; // 每次写入128字节，减小栈使用
    let total_size = size_of::<T>();
    let storage_ptr = storage as *const T as *const u8;
    
    for chunk_offset in (0..total_size).step_by(CHUNK_SIZE) {
        let chunk_size = core::cmp::min(CHUNK_SIZE, total_size - chunk_offset);
        // 使用固定大小的缓冲区，但只使用实际需要的部分
        let mut chunk = [0u8; CHUNK_SIZE];
        
        unsafe {
            ptr::copy_nonoverlapping(
                storage_ptr.add(chunk_offset),
                chunk.as_mut_ptr(),
                chunk_size
            );
        }
        
        write_flash(offset + chunk_offset as u32, &chunk[..chunk_size])?;
    }
    
    Ok(())
}

pub trait NvsStorage{
    fn read()->  Result<Self,FlashStorageError>  where Self: Sized;

    fn write(&self)-> Result<(), FlashStorageError>;
}


macro_rules! impl_storage {
    ($type:ty, $offset:expr) => {
        impl NvsStorage for $type {
            fn read() -> Result<Self,FlashStorageError> {
                let mut buffer = [0u8; size_of::<Self>()];
                let result = read_flash($offset as u32, &mut buffer);
                  match result {
                    Ok(_) => { Ok(deserialize_storage(&buffer)) }
                    Err(e) => {
                       Err(e)
                    }
                }
            }

            fn write(&self) -> Result<(), FlashStorageError> {
                let data = serialize_storage(self);
                write_flash($offset as u32, &data)
            }
        }
    };
}
const NVS_OFFSET:usize = 0x9000;

const VERSION_STORAGE_OFFSET:usize = NVS_OFFSET + 0x00;
const INIT_TAG:u32 = 0x1234abcb;//每次修改storage结构体后需要修改

#[derive(Debug,Default)]
pub struct VersionStorage{
    pub version:u32,
    pub init_tag:u32,
}

const WIFI_STORAGE_OFFSET:usize =  VERSION_STORAGE_OFFSET+ size_of::<VersionStorage>();


#[derive(Debug,Default)]
pub struct WifiStorage{
    pub wifi_ssid:heapless::String<32>,
    pub wifi_password:heapless::String<64>,
    pub wifi_finish:bool
}

const WEATHER_STORAGE_OFFSET:usize = WIFI_STORAGE_OFFSET+ size_of::<WifiStorage>();



//保存天气的app key 和 结果
#[derive(Debug,Default)]
pub struct WeatherStorage{
    pub token:heapless::String<64>,
    pub city:heapless::String<32>,
    pub sync_time_second:u64,
    pub weather_data:Option<DailyResult>
}

const SLEEP_STORAGE_OFFSET:usize = WEATHER_STORAGE_OFFSET + size_of::<WeatherStorage>();

//睡眠时间配置
#[derive(Debug,Default)]
pub struct SleepStorage{
    pub read_sleep_seconds:u64,  // 阅读睡眠时间（秒）
    pub weather_sleep_seconds:u64,  // 日历睡眠时间（秒）
}

const OTHER_STORAGE_OFFSET:usize = SLEEP_STORAGE_OFFSET + size_of::<SleepStorage>();

//保留
#[derive(Debug,Default)]
pub struct OtherStorage{
    pub data:heapless::String<64>
}
const HOLIDAY_OFFSET:usize = OTHER_STORAGE_OFFSET + size_of::<OtherStorage>();



//节假日数据
#[derive(Debug,Default)]
pub struct HolidayStorage{
    pub token:heapless::String<64>,
    pub sync_time_second:u64,
    pub holiday_response:Option<HolidayResponse> ,
}

const ERROR_LOG_OFFSET:usize = HOLIDAY_OFFSET + size_of::<HolidayStorage>();

#[derive(Debug,Default,Clone)]
pub struct ErrorLogStorage{
    pub error_count:u32,
    pub last_error:heapless::String<200>,
}


const TIMER_LOG_END_OFFSET:usize = ERROR_LOG_OFFSET + size_of::<ErrorLogStorage>();


// 为各个存储结构体实现 NvsStorage trait
impl_storage!(VersionStorage, VERSION_STORAGE_OFFSET);
impl_storage!(WifiStorage, WIFI_STORAGE_OFFSET);
impl_storage!(WeatherStorage, WEATHER_STORAGE_OFFSET);
impl_storage!(SleepStorage, SLEEP_STORAGE_OFFSET);
impl_storage!(OtherStorage, OTHER_STORAGE_OFFSET);
// HolidayStorage 使用手动实现，避免栈溢出
impl_storage!(ErrorLogStorage, ERROR_LOG_OFFSET);

// 为 HolidayStorage 手动实现 NvsStorage，使用分块写入避免栈溢出
impl NvsStorage for HolidayStorage {
    fn read() -> Result<Self, FlashStorageError> {
        let mut buffer = [0u8; size_of::<Self>()];
        let result = read_flash(HOLIDAY_OFFSET as u32, &mut buffer);
        match result {
            Ok(_) => { Ok(deserialize_storage(&buffer)) }
            Err(e) => {
               Err(e)
            }
        }
    }

    fn write(&self) -> Result<(), FlashStorageError> {
        // 使用分块写入，避免在栈上分配过大的数组
        write_storage_chunked(self, HOLIDAY_OFFSET as u32)
    }
}


pub static WIFI_INFO:Mutex<CriticalSectionRawMutex,Option<WifiStorage>>  =  Mutex::new(None);
pub static WEATHER_API:Mutex<CriticalSectionRawMutex,Option<WeatherStorage>>  =  Mutex::new(None);

pub async fn enter_process(){
    let version_storage = VersionStorage::read();
    match version_storage {
        Ok(v) => {
            if v.init_tag  != INIT_TAG {
                init_storage_area();
            }

            let wifi = WifiStorage::read().unwrap();
            WIFI_INFO.lock().await.replace(wifi);
        }
        Err(_) => {
            init_storage_area();
        }
    }
}

pub fn init_storage_area(){
    let mut version =  VersionStorage::default();
    version.version = 1;
    version.init_tag = INIT_TAG;
    version.write();

    let mut wifi =  WifiStorage::default();
    wifi.wifi_finish = false;
    wifi.write();

    WeatherStorage::default().write();
    let mut sleep_storage = SleepStorage::default();
    sleep_storage.read_sleep_seconds = 120;  // 默认阅读睡眠时间120秒
    sleep_storage.weather_sleep_seconds = 5;  // 默认日历睡眠时间5秒
    sleep_storage.write();
    OtherStorage::default().write();
    HolidayStorage::default().write();
    ErrorLogStorage::default().write();
}