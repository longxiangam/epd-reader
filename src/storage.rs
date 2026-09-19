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
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    let result = flash.write(flash_addr, bytes);

    match result {
        Ok(_) => {
            Ok(())
        }
        Err(e) => {
            println!("save fail：{:?}",e);
            Err(e)
        }
    }

}
pub fn read_flash(flash_addr:u32, bytes: &mut [u8]) -> Result<(), FlashStorageError> {
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    let result = flash.read(flash_addr,bytes);
    match result {
        Ok(_) => {
            Ok(())
        }
        Err(e) => {
            println!("read fail：{:?}",e);
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
const INIT_TAG:u32 = 0x1234abce;//每次修改storage结构体后需要修改

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

//股票配置：最多 6 支（代码 + 名称），selected 为当前选用索引
#[derive(Clone, Default, Debug)]
pub struct StockEntry{
    pub code:heapless::String<16>,
    pub name:heapless::String<32>,
}

#[derive(Clone, Debug)]
pub struct StockStorage{
    pub entries:[StockEntry;6],
    pub count:u8,       // 实际配置数量 0..=6
    pub selected:u8,    // 当前选用索引 0..count
}
impl Default for StockStorage{
    fn default()->Self{
        Self{
            entries:[
                StockEntry::default(),StockEntry::default(),StockEntry::default(),
                StockEntry::default(),StockEntry::default(),StockEntry::default(),
            ],
            count:0,
            selected:0,
        }
    }
}
const STOCK_STORAGE_OFFSET:usize = TIMER_LOG_END_OFFSET;

const DISPLAY_STORAGE_OFFSET:usize = STOCK_STORAGE_OFFSET + size_of::<StockStorage>();

const READING_SETTINGS_OFFSET:usize = DISPLAY_STORAGE_OFFSET + size_of::<DisplayStorage>();

/// 屏幕显示配置：快刷多少次后触发一次全刷（清残影）。
/// 独立分区追加在末尾，不动既有结构 → 既有数据不偏移、不丢失，无需 bump INIT_TAG。
/// 旧固件未写入此分区时读取为 flash 原始字节，由 display 侧 clamp 到平台默认值。
#[derive(Debug,Default,Clone,Copy)]
pub struct DisplayStorage{
    /// 0 表示未配置，display 侧回退到平台默认（epd2in7=20，其余=5）
    pub full_refresh_times:u32,
}

/// 阅读排版设置（跨会话持久化）：字号、行距、旋转、字体文件名。
/// 追加在末尾，不动既有结构。
#[derive(Debug, Clone, Copy)]
pub struct ReadingSettings {
    pub ttf_px: u8,
    pub line_gap: u8,
    pub rotate: u8,
    /// null 结尾的字体文件名（如 b"font.ttf\0..."）
    pub font_name: [u8; 32],
}

impl Default for ReadingSettings {
    fn default() -> Self {
        let mut font_name = [0u8; 32];
        let name = b"font.ttf";
        font_name[..name.len()].copy_from_slice(name);
        Self { ttf_px: 20, line_gap: 1, rotate: 0, font_name }
    }
}

/// v22 三键 ADC 校准数据（分压电阻有个体差异，阈值必须实测标定）。
/// 追加在末尾，不动既有结构。magic 无效/间隔不足 → 开机进入校准界面。
#[derive(Debug, Clone, Copy)]
pub struct AdcCalStorage {
    pub magic: u32, // 0x41444331 = "ADC1"，有效标志
    pub idle: u16,  // 未按下时的 ADC 均值（上拉 → 高）
    pub key1: u16,  // 按键1 按住时的 ADC 均值
    pub key2: u16,
    pub key3: u16,
}

impl Default for AdcCalStorage {
    fn default() -> Self {
        Self { magic: 0, idle: 0, key1: 0, key2: 0, key3: 0 }
    }
}

impl AdcCalStorage {
    /// "ADC2"：按键引脚换到 GPIO4 后分压点变化，旧校准数据作废，需重新校准
    pub const MAGIC: u32 = 0x4144_4332;
    /// 判定相邻键值最小间隔（12bit ADC）：小于此值视为无法可靠区分。
    pub const MIN_GAP: i32 = 120;

    /// 校准数据有效性：magic 正确 + idle 与三键值两两间隔足够。
    pub fn valid(&self) -> bool {
        if self.magic != Self::MAGIC || self.idle == 0 {
            return false;
        }
        let mut v = [self.idle as i32, self.key1 as i32, self.key2 as i32, self.key3 as i32];
        v.sort_unstable();
        for w in v.windows(2) {
            if w[1] - w[0] < Self::MIN_GAP {
                return false;
            }
        }
        true
    }
}

impl ReadingSettings {
    /// 取 font_name 的 &str（到首个 \0 或全长）。
    pub fn font_name_str(&self) -> &str {
        let len = self.font_name.iter().position(|&b| b == 0).unwrap_or(self.font_name.len());
        core::str::from_utf8(&self.font_name[..len]).unwrap_or("font.ttf")
    }
}


// 为各个存储结构体实现 NvsStorage trait
impl_storage!(VersionStorage, VERSION_STORAGE_OFFSET);
impl_storage!(WifiStorage, WIFI_STORAGE_OFFSET);
impl_storage!(WeatherStorage, WEATHER_STORAGE_OFFSET);
impl_storage!(SleepStorage, SLEEP_STORAGE_OFFSET);
impl_storage!(OtherStorage, OTHER_STORAGE_OFFSET);
// HolidayStorage 使用手动实现，避免栈溢出
impl_storage!(ErrorLogStorage, ERROR_LOG_OFFSET);
impl_storage!(StockStorage, STOCK_STORAGE_OFFSET);
impl_storage!(DisplayStorage, DISPLAY_STORAGE_OFFSET);
impl_storage!(ReadingSettings, READING_SETTINGS_OFFSET);

const ADC_CAL_OFFSET: usize = READING_SETTINGS_OFFSET + size_of::<ReadingSettings>();
impl_storage!(AdcCalStorage, ADC_CAL_OFFSET);

/// 最后所在页（跨深睡唤醒恢复）。IO（RTCIO）唤醒时 rtc_fast 状态不可靠
/// （BOOTED/PAGE_INDEX 会丢，设备落到默认页），故页面变化时同步落到 NVS。
#[derive(Debug, Clone, Copy)]
pub struct SleepPageStorage {
    pub magic: u32, // 0x50414745 = "PAGE"
    pub page: i32,  // 主网格页索引，-1=网格
}

impl Default for SleepPageStorage {
    fn default() -> Self {
        Self { magic: 0, page: -1 }
    }
}

impl SleepPageStorage {
    pub const MAGIC: u32 = 0x5041_4745;
    /// 哨兵值：上次处于阅读模式（区别于"网格选中电子书页"，后者冷启动不该进阅读模式）
    pub const PAGE_READING: i32 = 100;

    /// 读取有效值；未初始化/非法返回 None
    pub fn valid_page(&self) -> Option<i32> {
        if self.magic != Self::MAGIC
            || !(self.page == Self::PAGE_READING || (-1..=6).contains(&self.page))
        {
            return None;
        }
        Some(self.page)
    }
}

const SLEEP_PAGE_OFFSET: usize = ADC_CAL_OFFSET + size_of::<AdcCalStorage>();
impl_storage!(SleepPageStorage, SLEEP_PAGE_OFFSET);

/// 读取睡眠前最后所在页（NVS）。None=无记录
pub fn read_sleep_page() -> Option<i32> {
    SleepPageStorage::read().ok().and_then(|sp| sp.valid_page())
}

/// 写入睡眠前最后所在页（值未变化时跳过写入，避免磨损）
pub fn write_sleep_page(page: i32) {
    if read_sleep_page() == Some(page) {
        return;
    }
    let mut sp = SleepPageStorage::read().unwrap_or_default();
    sp.magic = SleepPageStorage::MAGIC;
    sp.page = page;
    let _ = sp.write();
}

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
    StockStorage::default().write();
    // 0 = 未配置，display 侧回退到平台默认（无需在此处区分 feature）
    DisplayStorage::default().write();
    ReadingSettings::default().write();
    AdcCalStorage::default().write();
}