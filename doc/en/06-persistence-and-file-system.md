# Part 6: Data Persistence and File Systems

> 中文版: [../06-数据持久化与文件系统.md](../06-数据持久化与文件系统.md)

> Building an ESP32-C3 E-Paper Reader in Rust · Article Series

## Introduction

Embedded devices must keep data across power cycles — WiFi credentials, weather cache, reading progress, layout settings, error logs, even "the current time" itself. On a PC you might reach for SQLite or a file system, but on an ESP32-C3 with only 4MB of Flash every byte has to be spent carefully.

This project uses three storage media: the NVS partition of the ESP32's internal Flash for configuration data, a MicroSD card over SPI for bulk storage (e-books, images, reading progress), and a PCF8563 hardware RTC (kept alive by a CR2032 backup battery) for time. This article dives into how each of them is implemented.

## 1. Flash Storage: Zero-Copy Serialization

### 1.1 The Flash Access Interface

The ESP32's Flash is accessed through the `esp-storage` crate, which implements the `ReadStorage` and `Storage` traits of `embedded_storage`:

```rust
// src/storage.rs:11-40
pub fn write_flash(flash_addr: u32, bytes: &[u8]) -> Result<(), FlashStorageError> {
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    let result = flash.write(flash_addr, bytes);
    // On Err, println prints "save fail" and the error is returned unchanged
    match result { Ok(_) => Ok(()), Err(e) => { println!("save fail：{:?}", e); Err(e) } }
}

pub fn read_flash(flash_addr: u32, bytes: &mut [u8]) -> Result<(), FlashStorageError> {
    let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
    let mut flash = FlashStorage::new(flash);
    let result = flash.read(flash_addr, bytes);
    match result { Ok(_) => Ok(()), Err(e) => { println!("read fail：{:?}", e); Err(e) } }
}
```

`FLASH::steal()` is an API added in esp-hal v1.x — it bypasses the ownership system to obtain a reference to the Flash peripheral. This is acceptable because Flash operations do not need the kind of exclusive access SPI requires (it uses the internal bus), and exactly one place in the program touches the Flash.

### 1.2 Zero-Copy Serialization

The project uses an extremely simple serialization scheme — writing the struct's in-memory representation straight into Flash:

```rust
// src/storage.rs:43-49
fn serialize_storage<T>(storage: &T) -> [u8; size_of::<T>()] {
    unsafe { ptr::read(storage as *const _ as *const [u8; size_of::<T>()]) }
}

fn deserialize_storage<T>(data: &[u8]) -> T {
    unsafe { ptr::read(data.as_ptr() as *const T) }
}
```

`serialize_storage` reinterprets the struct's memory bytes as `[u8; N]`, and `deserialize_storage` does the reverse. In essence the struct's raw memory bytes are read out and written to Flash directly, implemented with `ptr::read` / pointer reinterpretation.

**Advantages**: zero heap allocation, zero copies, extremely fast serialization.
**Limitation**: the struct's memory layout must match what is in Flash. If the struct definition changes (fields added/removed, types changed), the old data in Flash can no longer be deserialized correctly.

### 1.3 The Storage Layout

The NVS partition (starting at 0x9000) is divided into contiguous storage blocks, one data type per block:

```rust
// src/storage.rs (base NVS_OFFSET = 0x9000; blocks are adjacent, each offset is
// the previous offset + size_of::<previous struct>())
const VERSION_STORAGE_OFFSET = NVS_OFFSET + 0x00;
// VersionStorage { version: u32, init_tag: u32 }

const WIFI_STORAGE_OFFSET = VERSION_STORAGE_OFFSET + size_of::<VersionStorage>();
// WifiStorage { wifi_ssid: String<32>, wifi_password: String<64>, wifi_finish: bool }

const WEATHER_STORAGE_OFFSET = WIFI_STORAGE_OFFSET + size_of::<WifiStorage>();
// WeatherStorage { token: String<64>, city: String<32>, sync_time_second: u64, weather_data: Option<DailyResult> }

const SLEEP_STORAGE_OFFSET = WEATHER_STORAGE_OFFSET + size_of::<WeatherStorage>();
// SleepStorage { read_sleep_seconds: u64, weather_sleep_seconds: u64 }

const OTHER_STORAGE_OFFSET = SLEEP_STORAGE_OFFSET + size_of::<SleepStorage>();
// OtherStorage { data: String<64> }   // reused: first char holds the default home page
                                        // (0=e-books/1=weather/2=calendar/3=stocks/9=home grid)

const HOLIDAY_OFFSET = OTHER_STORAGE_OFFSET + size_of::<OtherStorage>();
// HolidayStorage { token: String<64>, sync_time_second: u64, holiday_response: Option<HolidayResponse> }

const ERROR_LOG_OFFSET = HOLIDAY_OFFSET + size_of::<HolidayStorage>();
// ErrorLogStorage { error_count: u32, last_error: String<200> }

const STOCK_STORAGE_OFFSET = ERROR_LOG_OFFSET + size_of::<ErrorLogStorage>();
// StockStorage { entries: [StockEntry; 6], count: u8, selected: u8 }
//   StockEntry { code: String<16>, name: String<32> }

const DISPLAY_STORAGE_OFFSET = STOCK_STORAGE_OFFSET + size_of::<StockStorage>();
// DisplayStorage { full_refresh_times: u32 }   // 0 = unconfigured; the display side
                                                // clamps to the platform default

const READING_SETTINGS_OFFSET = DISPLAY_STORAGE_OFFSET + size_of::<DisplayStorage>();
// ReadingSettings { ttf_px: u8, line_gap: i8, rotate: u8, font_name: [u8; 32] }
//   Defaults: 20 / 1 / 0 / "font.ttf"; read when opening a book and clamped
//   (px 12..=40, gap ±10, rotate %4)

const SYNC_STAMP_OFFSET = READING_SETTINGS_OFFSET + size_of::<ReadingSettings>();
// SyncStampStorage { magic: u32 /*"SYND"*/, time_sync_sec: u64, holiday_sync_sec: u64, show_time: u8 }
//   Manual time-sync / holiday-update timestamps + the clock display toggle on the
//   weather/calendar pages (occupies the slot of the removed ADC calibration feature)

const SLEEP_PAGE_OFFSET = SYNC_STAMP_OFFSET + size_of::<SyncStampStorage>();
// SleepPageStorage { magic: u32 /*"PAGE"*/, page: i32 }
//   Last page before sleep: -1..=6 is a grid page index; sentinel 100 = reading mode
//   (used to restore the scene after deep sleep / power loss)

const READING_FLAGS_OFFSET = SLEEP_PAGE_OFFSET + size_of::<SleepPageStorage>();
// ReadingFlagsStorage { magic: u32 /*"PGSW"*/, swap_keys: u8 }   // swap reading page-turn keys

const STROKE_CONFIG_OFFSET = READING_FLAGS_OFFSET + size_of::<ReadingFlagsStorage>();
// StrokeConfigStorage { magic: u32 /*"STKE"*/, weight_x10: u8 }   // stroke bolding ×10, 10..=30
```

Each block's offset is the previous block's offset plus its size — a simple but effective sequential allocation. All later additions (`StockStorage`, `DisplayStorage`, `ReadingSettings`, … up to `StrokeConfigStorage`) are **appended at the end without touching existing structures** — existing data neither shifts nor is lost, so `INIT_TAG` never needs bumping.

New blocks also face the problem "the Flash was never written and reads back 0xFF/garbage", handled in two ways:

- **Magic-carrying blocks** (`SyncStampStorage` / `SleepPageStorage` / `ReadingFlagsStorage` / `StrokeConfigStorage`): reads verify the magic; invalid reads fall back to defaults (e.g. `read_stroke_weight()` returns 10 whenever the magic is invalid or the coefficient is out of range);
- **Blocks without magic** (e.g. `DisplayStorage`, `ReadingSettings`): users clamp fields to legal ranges (out-of-range falls back to defaults), and `StockStorage::default` fills 6 empty entries.

The write side is equally careful: `write_sleep_page()` skips the write when the value is unchanged, reducing Flash wear.

### 1.4 The impl_storage! Macro

To avoid reimplementing read/write for every storage type, the project defines a macro:

```rust
// src/storage.rs:83-103
macro_rules! impl_storage {
    ($type:ty, $offset:expr) => {
        impl NvsStorage for $type {
            fn read() -> Result<Self, FlashStorageError> {
                let mut buffer = [0u8; size_of::<Self>()];
                read_flash($offset as u32, &mut buffer)?;
                Ok(deserialize_storage(&buffer))
            }
            fn write(&self) -> Result<(), FlashStorageError> {
                let data = serialize_storage(self);
                write_flash($offset as u32, &data)
            }
        }
    };
}

// One line implements read/write for a type
impl_storage!(VersionStorage, VERSION_STORAGE_OFFSET);
impl_storage!(WifiStorage, WIFI_STORAGE_OFFSET);
impl_storage!(WeatherStorage, WEATHER_STORAGE_OFFSET);
impl_storage!(SleepStorage, SLEEP_STORAGE_OFFSET);
// ... Other / ErrorLog / Stock / Display / ReadingSettings / SyncStamp /
//     SleepPage / ReadingFlags / StrokeConfig — 13 in total
```

The `NvsStorage` trait defines the unified read/write interface:

```rust
pub trait NvsStorage {
    fn read() -> Result<Self, FlashStorageError> where Self: Sized;
    fn write(&self) -> Result<(), FlashStorageError>;
}
```

The single exception is `HolidayStorage` — it implements `NvsStorage` manually, with a chunked write (see 1.5).

### 1.5 Chunked Writes: Avoiding Stack Overflow

`HolidayStorage` contains a large `HolidayResponse` struct; serializing it as a whole could blow the stack. The project implements chunked writing for it:

```rust
// src/storage.rs:52-74
fn write_storage_chunked<T>(storage: &T, offset: u32) -> Result<(), FlashStorageError> {
    const CHUNK_SIZE: usize = 128;  // only 128 bytes handled at a time
    let total_size = size_of::<T>();
    let storage_ptr = storage as *const T as *const u8;

    for chunk_offset in (0..total_size).step_by(CHUNK_SIZE) {
        let chunk_size = core::cmp::min(CHUNK_SIZE, total_size - chunk_offset);
        let mut chunk = [0u8; CHUNK_SIZE];  // fixed-size stack buffer

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
```

The key: no matter how large the struct is, only a 128-byte buffer is ever allocated on the stack. Writing chunk by chunk avoids the risk of `serialize_storage` allocating `[u8; size_of::<T>()]` on the stack. Only `HolidayStorage`'s `write()` is chunked; `read()` is still a single-block read (the read-side buffer is the caller's local variable of fixed size, a manageable risk).

### 1.6 Version Check and Initialization

At boot the initialization tag in Flash is checked; if it is absent or mismatched, all storage areas are re-initialized:

```rust
// src/storage.rs:470-485
pub async fn enter_process() {
    let version_storage = VersionStorage::read();
    match version_storage {
        Ok(v) => {
            if v.init_tag != INIT_TAG {  // 0x1234_abce
                init_storage_area();     // update INIT_TAG after struct changes
            }
            let wifi = WifiStorage::read().unwrap();
            WIFI_INFO.lock().await.replace(wifi);
        }
        Err(_) => {
            init_storage_area();  // first boot
        }
    }
}
```

`INIT_TAG` is a magic constant. It must be changed whenever a storage struct is modified, so the next boot detects the mismatch and re-initializes. `init_storage_area()` writes defaults for the whole chain (`SleepStorage` defaults to 120s reading / 5s calendar sleep; `ReadingSettings` defaults to 20px / gap 1 / font.ttf …). The magic-carrying `SleepPageStorage` / `ReadingFlagsStorage` / `StrokeConfigStorage` are not written there — they self-heal via magic validation and need no explicit initialization.

## 2. The SD Card File System

### 2.1 Mounting and Type Aliases

The SD card is operated over SPI with the `embedded-sdmmc` crate. To tame the generics the project predefines type aliases:

```rust
// src/sd_mount.rs:31-37
pub type SdCsPin = Output<'static>;
pub type ActualSpi = CriticalSectionDevice<'static, Spi<'static, esp_hal::Blocking>, SdCsPin, NoDelay>;
pub type ActualSdCard = SdCard<&'static mut ActualSpi, Delay>;
pub type ActualVolumeManager = VolumeManager<ActualSdCard, TimeSource>;
pub type ActualVolume<'a> = Volume<'a, ActualSdCard, TimeSource, 4, 4, 1>;
pub type ActualDirectory<'a> = Directory<'a, ActualSdCard, TimeSource, 4, 4, 1>;
pub type ActualFile<'a> = File<'a, ActualSdCard, TimeSource, 4, 4, 1>;
```

The generic parameters `4, 4, 1` are the maximum number of volumes, open directories, and open files respectively. In a resource-constrained environment these need careful control.

The SD card is mounted in `main.rs` and stored in a global Mutex:

```rust
// src/main.rs:287-304
let sdcard = SdCard::new_with_options(spi_bus_sd, embassy_time::Delay, AcquireOpts {
    use_crc: false,           // CRC checking off (some cards are incompatible)
    acquire_retries: 50,      // 50 init retries
});
let volume_mgr = VolumeManager::new(sdcard, crate::sd_mount::TimeSource);
let sd_mount = SdMount::new(volume_mgr);
SD_MOUNT.lock().await.replace(sd_mount);
// The SD must be talked to once after power-up, or it interferes with display communication
if let Some(ref mut sd) = *SD_MOUNT.lock().await {
    match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(_) => println!("[sd] volume open ok"),
        Err(e) => println!("[sd] open_volume fail: {:?}", e),
    }
}
```

`TimeSource` returns all-zero timestamps — the FAT timestamp fields just have to be legal; the device has no usable real-time clock to write into the SD (the PCF8563 does keep time, but no `TimeSource` adapter was written for it).

### 2.2 Long File Name (LFN) Support

Chinese book titles far exceed FAT's 8.3 short-name limit, so LFN is a hard requirement. Upstream `embedded-sdmmc` only supported LFN **reading**, not **creation**, so this project uses its own fork:

```toml
# Cargo.toml
embedded-sdmmc = { git = "https://github.com/longxiangam/embedded-sdmmc-rs", branch = "lfn-create" }
```

The fork completes LFN directory-entry writing inside the library (generating a unique `~`-containing short name + building UTF-16LE LFN entries + checksums) and exposes one plain new API:

```rust
// src/sd_mount.rs:299-315
/// Open/create a file by long file name (e.g. "三体.log") — LFN creation happens
/// inside the forked library
pub fn open_log_file<'a>(
    books_dir: &'a mut ActualDirectory,
    book_name: &str,
    mode: Mode,
) -> Result<ActualFile<'a>, SdError> {
    let log_lfn = format!("{}.log", book_name);
    books_dir
        .open_long_name_file_in_dir(&log_lfn, mode)
        .map_err(|_| SdError::FileNotFound)
}

/// Pure long-name open (used when uploading e-books/images)
pub fn open_file_by_name<'a>(books_dir: &'a mut ActualDirectory, file_name: &str,
                             mode: Mode) -> Result<ActualFile<'a>, SdError> {
    books_dir.open_long_name_file_in_dir(file_name, mode).map_err(|_| SdError::FileNotFound)
}
```

An early version hand-wrote the three LFN write stages at the application level (scan the directory for a unique `BK0001~1.TXT` short name → create the file by short name so FAT allocates clusters → rewrite the directory sector to insert LFN entries); after moving into the fork all of that was deleted and the application layer keeps only business logic.

The **read** side uses the library's own `iterate_dir_lfn` + `LfnBuffer` (a 512-byte stack buffer) to walk a directory, delivering the short-name entry and the long-name string in a single callback:

```rust
// src/sd_mount.rs:112-162 (get_books excerpt)
pub fn get_books(books_dir: &mut ActualDirectory)
    -> Result<Vec<String<BOOK_NAME_MAX>, 40>, SdError>
{
    let mut storage = [0u8; 512];
    let mut lfn_buffer = LfnBuffer::new(&mut storage);
    books_dir.iterate_dir_lfn(&mut lfn_buffer, |directory, lfn| {
        if let Some(lfn) = lfn {
            // Long name is valid UTF-8: skip hidden entries (starting with "."),
            // over-long ones, keep only .txt (case-insensitive)
            if lfn.starts_with('.') { return ControlFlow::Continue(()); }
            if !ends_with_ignore_case(lfn, ".txt") { return ControlFlow::Continue(()); }
            // strip the extension and push onto the book list
        } else if directory.name.extension() == b"TXT" {
            // legacy files without an LFN fall back to the 8.3 short name
        }
        ControlFlow::Continue(())
    }).unwrap();
    Ok(books)
}
```

Around it sits a set of directory/file helpers:

- `get_subdirs()`: lists subdirectories (the book list supports one level of folder grouping);
- `get_images()`: the same logic for `.bmp` images;
- `find_entry_by_name()`: case-insensitive lookup of a directory entry by name (delete and open both need the short-name entry first);
- `url_decode()` + `BOOK_NAME_MAX = 150`: provisioning upload names are percent-decoded first (`%XX`/`+`); Chinese titles up to ~40 characters;
- the `.idx` helpers (`derive_short_name` / `open_idx_file` / `idx_file_exists`): leftovers from the old page-index era, now used only to clean up old `.idx` files when deleting a book — new code never writes them.

## 3. Reading-Data Persistence: .log and Resume

### 3.1 From .idx to .log

The earliest reader was "bitmap font + pre-paginated": opening a book scanned the whole text and wrote each page's byte offset into a `.idx` index file (an array of `u32`s), loaded on demand while paging. Once the reader switched to TTF vector fonts (with adjustable font size / line gap / font file — Part 10), pre-pagination lost its premise — change a layout parameter and every page boundary moves. Paging is now **byte offsets + on-the-fly typesetting**, so `.idx` is no longer needed; the only thing persisted is "how far did I read", the `.log` file.

### 3.2 The .log Format and TxtReader

A `.log` is named after the book's long name (`三体.log`) and contains a big-endian `u32` array:

- `[0]`: the byte offset of the last reading position;
- `[1..]`: byte offsets of bookmarks (up to 100, deduplicated).

The read/write logic lives in `TxtReader` in `src/epd2in9_txt.rs`:

```rust
// src/epd2in9_txt.rs:61-99 (save_log excerpt)
/// is_favorite=true appends a bookmark (deduplicated), otherwise updates [0]
/// (last reading position)
pub fn save_log(my_file: &mut FileObject, log_vec: &mut Vec<u32, LOG_VEC_MAX>,
                page: u32, is_favorite: bool) {
    if is_favorite {
        // only scan the bookmark area (index 1+); no dedup against "last position"
        let already = log_vec.iter().skip(1).any(|&p| p == page);
        if !already && log_vec.len() < LOG_VEC_MAX { log_vec.push(page); }
    } else if log_vec.len() == 0 {
        log_vec.push(page);
    } else {
        log_vec[0] = page;
    }
    // write every u32 back to the SD in big-endian order
}
```

The related constants live in the same file: `LOG_VEC_MAX = 100` (bookmark cap), `PAGES_VEC_MAX = 1000`, `ONE_PAGE_CONTENT_LEN = 2000` (per-page text buffer in bytes), plus the per-screen-feature `WIDTH/HEIGHT/LINES_NUM`. The book-list page also batch-reads every book's `.log[0]` to render progress like "32% read" next to each title.

Every path that turns a page, jumps, or exits reading writes `.log[0]` (`save_log_raw` writes the whole Vec back verbatim), so pulling the card or losing power at any moment costs at most the current page.

### 3.3 The Cross-Sleep Resume Chain

"Close the cover tonight, continue tomorrow" relies on three tiers of data:

1. **rtc_fast (survives deep sleep, not power loss)**: `READING_MODE` (boot straight into reading), `TTF_RESUME_OFFSET` (the current book's byte offset), `LIST_SUBDIR/LIST_BOOK_IDX` (book-list directory and cursor). After a deep-sleep wake-up this restores in an instant, without touching the SD card;
2. **NVS `SleepPageStorage`**: the page number is written whenever the page changes (sentinel 100 = reading mode). In extreme cases such as IO wake-ups rtc_fast is lost; main consults NVS at boot for "was in reading" and takes the reading-mode reboot branch directly (no WiFi init);
3. **SD `.log[0]`**: a cold boot opening a book restores the last position from `.log[0]` (clamped into the file); if rtc_fast holds a newer `TTF_RESUME_OFFSET` (deep-sleep resume), that wins and `.log[0]` is skipped.

The result of the three-tier fallback: deep-sleep resume never reads the card, and a power cycle returns to the same book and position.

## 4. Persisting Time: the PCF8563 Hardware RTC

### 4.1 The I2C Bus Wrapper

The v2 board carries an I2C bus (GPIO9=SDA / GPIO21=SCL, 100kHz — the PCF8563's safe limit) with two devices: the AHT20 temperature/humidity sensor (0x38) and the PCF8563 RTC (0x51). The bus lives behind a global Mutex, and drivers access it only through the helpers in `i2c_bus.rs`:

```rust
// src/i2c_bus.rs
pub static I2C: Mutex<CriticalSectionRawMutex, Option<I2cDev>> = Mutex::new(None);

pub async fn write(addr: u8, data: &[u8]) -> Result<(), ()> { /* lock bus → dev.write */ }
pub async fn write_read(addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ()> { /* write register address, then read */ }
```

The old board (board_v1) has no I2C peripheral: `init_i2c` returns None and PCF calls naturally short-circuit.

### 4.2 Reading and Writing the PCF8563

`pcf8563.rs` treats the PCF8563 as the system clock source: the registers for seconds(0x02)/minutes/hours/day/weekday/month/year are all BCD, and time is stored and read as **UTC** (screen display goes through `Clock::local`, uniformly +8h):

```rust
// src/pcf8563.rs:27-47 (read_time)
pub async fn read_time() -> Option<i64> {
    let mut b = [0u8; 7];
    i2c_bus::write_read(ADDR /*0x51*/, &[REG_SECONDS], &mut b).await.ok()?;
    if b[0] & 0x80 != 0 {
        return None;  // VL=1: the backup battery lost power / first power-up; time untrustworthy
    }
    // BCD decode + validity checks (month 1..=12, hour 0..23, ...)
    // assemble a PrimitiveDateTime → Unix seconds (UTC)
}

pub async fn write_time(ts: i64) -> bool {
    // 1) write Control_1: STOP=1 pauses ticking (guards against rollback tearing);
    //    failure returns false immediately
    if i2c_bus::write(ADDR, &[0x00, 0x20]).await.is_err() { return false; }
    // 2) write 7 BCD registers starting at 0x02 (seconds)
    let ok = i2c_bus::write(ADDR, &[REG_SECONDS, sec, min, hour, day, weekday, month, year]).await.is_ok();
    // 3) STOP=0 resumes ticking
    let _ = i2c_bus::write(ADDR, &[0x00, 0x00]).await;
    ok
}
```

The chip is also the deep-sleep timed-wake source: the countdown timer (`arm_timer_secs/minutes`, INT pin pulled low to wake) and the absolute-time alarm (`arm_alarm_at` — the PCF keeps time on its coin-cell-driven 32k crystal, far more accurate than the main SoC's deep-sleep slow clock) both live here — covered in Part 7.

### 4.3 The Time Restoration Chain: PCF First, rtc_fast as Fallback

`ntp_worker` in `worldtime.rs` restores the `Clock` at every boot with the following priority:

```rust
// src/worldtime.rs:392-415 (excerpt)
#[embassy_executor::task]
pub async fn ntp_worker() {
    let clock = CLOCK_CELL.init(Clock::new());
    // 1) Restore from the PCF8563 first (backup battery holds it; authoritative)
    let restored = pcf_restore(clock).await;
    if !restored {
        // 2) Fall back to a rtc_fast estimate: pre-sleep timestamp + sleep duration
        //    The restored value also goes through a 2020..2100 sanity check;
        //    if it fails, wait for NTP to re-sync instead of setting a wrong time
    }
    loop {
        // More than 3600s since the last sync → use_wifi + ntp_request
        // 3) On success: update CLOCK_SYNC_TIME_SECOND, write UTC into the PCF8563
        //    + SyncStampStorage::write_time_stamp (the manual-sync record)
        // Failure backoff: repeated failures 1s→10s; no network 10s→3600s
    }
}
```

When `pcf_restore` succeeds it also fills in `CLOCK_SYNC_TIME_SECOND` in rtc_fast — that flag is lost on IO wake-ups, and without it the weather page would misjudge "never synced" and NTP would run a pointless extra round. With this chain in place, network-less modes such as reading (`init_clock_pcf`, where the PCF is the only time source), deep-sleep drift, and battery-swap power loss no longer lose time; the stock page's day-boundary detection no longer breaks from a stale clock.

### 4.4 SyncStampStorage: Persisted Sync Metadata

On the Flash side, `SyncStampStorage` (magic `"SYND"`) records three fields: the manual time-sync timestamp, the holiday-update timestamp, and the clock-display toggle for the weather/calendar pages. It is the persistent "metadata about time" — the settings page's "last synced: n days ago" display and judging data staleness after long NTP failures both read from here. The PCF8563 answers "what time is it now"; `SyncStampStorage` answers "how long since the last sync"; they complement each other.

## Summary

| Topic | Key Point |
|-------|-----------|
| Flash access | `esp-storage` + `FLASH::steal()` |
| Zero-copy serialization | `ptr::read` writes struct memory straight into Flash |
| Storage layout | 13 blocks allocated sequentially in the NVS partition; new blocks append only |
| Magic validation | magic-carrying blocks self-heal; the rest rely on clamping |
| Chunked writes | 128-byte chunks avoid stack overflow (`HolidayStorage`) |
| impl_storage! macro | One line implements the read/write trait for a type |
| SD card | `embedded-sdmmc` fork (lfn-create branch) + SPI |
| LFN support | `open_long_name_file_in_dir` creates files directly by Chinese long name |
| Reading progress | `.log` file: `[0]` last position + bookmark array, big-endian u32 |
| Resume chain | rtc_fast (deep sleep) → NVS page (IO wake-up) → SD `.log` (power loss) |
| Time persistence | PCF8563 (CR2032-backed, UTC/BCD) + `SyncStampStorage` |

The core idea: **in a resource-constrained environment, solve persistence the most direct way possible**. Zero-copy serialization is not the most elegant design, but it is the one that fits embedded best — fast, small, memory-thrifty. And "pick the cheapest non-volatile medium per data class" (configuration in Flash, bulk files on SD, time in a hardware RTC) works better than stretching one storage to its limits.

In the next article we discuss low-power design — how the ESP32-C3 achieves long standby through deep sleep.

---

> Previous: [Part 5: WiFi Networking and HTTP Communication](../05-WiFi网络与HTTP通信.md) · Next: [Part 7: Low-Power Design and Power Management](../07-低功耗设计与电源管理.md)
