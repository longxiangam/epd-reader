use alloc::string::ToString;
use alloc::format;
use core::str::FromStr;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{Directory, File, LfnBuffer, Mode, SdCard, ShortFileName, Volume, VolumeManager};
use core::ops::ControlFlow;
use embassy_time::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::Spi;
use esp_println::println;
use heapless::{String, Vec};
use crate::sd_mount::SdError::FileNotFound;

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
pub type SdCsPin = Output<'static>;
pub type ActualSpi = CriticalSectionDevice<'static, Spi<'static, esp_hal::Blocking>, SdCsPin, embedded_hal_bus::spi::NoDelay>;
pub type ActualSdCard = SdCard<&'static mut ActualSpi, Delay>;
pub type ActualVolumeManager = VolumeManager<ActualSdCard, TimeSource>;
pub type ActualVolume<'a> = Volume<'a,ActualSdCard, TimeSource,4,4,1>;
pub type ActualDirectory<'a> = Directory<'a,ActualSdCard, TimeSource,4,4,1>;
pub type ActualFile<'a> = File<'a,ActualSdCard, TimeSource,4,4,1>;

//pub static SDCARD_VOLUME_MGR_REF:Mutex<CriticalSectionRawMutex,Option<ActualVolumeManager<'static, SdCsPin>>> = Mutex::new(None);

pub static  SD_MOUNT:Mutex<CriticalSectionRawMutex,Option<SdMount>> = Mutex::new(None);

#[derive(Debug)]
pub enum SdError{
    FileNotFound,
}

pub struct SdMount{
    pub volume_manager: ActualVolumeManager,

}

/// Max bytes for a book name (Chinese chars are 3 bytes each in UTF-8).
/// Allows ~40 Chinese characters (120 bytes) + some margin.
pub const BOOK_NAME_MAX: usize = 150;

/// URL percent-decode: converts `%XX` → byte, `+` → space.
/// Returns the decoded string, or None if the encoding is invalid.
pub fn url_decode(input: &str) -> Option<alloc::string::String> {
    let mut bytes = alloc::vec::Vec::new();
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            bytes.push(b' ');
        } else if b == b'%' {
            let hi = chars.next()?;
            let lo = chars.next()?;
            let high = (hi as char).to_digit(16)?;
            let low = (lo as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(b);
        }
    }
    alloc::string::String::from_utf8(bytes).ok()
}

/// Case-insensitive string equality for ASCII portions only.
/// Chinese characters are compared as-is (no case mapping).
fn eq_ignore_case(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.chars().zip(b.chars()).all(|(ca, cb)| {
        if ca.is_ascii() && cb.is_ascii() {
            ca.to_ascii_uppercase() == cb.to_ascii_uppercase()
        } else {
            ca == cb
        }
    })
}

/// Check if a string ends with a given suffix, ASCII case-insensitive.
fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    let suffix_chars: alloc::vec::Vec<char> = suffix.chars().collect();
    let s_chars: alloc::vec::Vec<char> = s.chars().collect();
    if s_chars.len() < suffix_chars.len() {
        return false;
    }
    let tail = &s_chars[s_chars.len() - suffix_chars.len()..];
    tail.iter().zip(suffix_chars.iter()).all(|(a, b)| {
        a.to_ascii_uppercase() == b.to_ascii_uppercase()
    })
}

impl SdMount{
    pub fn new(volume_manager: ActualVolumeManager)->Self{
        Self{ volume_manager}
    }


    pub  fn get_books(books_dir:&mut ActualDirectory)->Result<Vec<String<BOOK_NAME_MAX>,40>,SdError>
    {
        let mut books:Vec<String<BOOK_NAME_MAX>,40> = Vec::new();

        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        books_dir.iterate_dir_lfn(&mut lfn_buffer,|directory, lfn|{

            println!(
                "{:12} {:9} {} {} {:08X?} {:5?}",
                directory.name,
                directory.size,
                directory.ctime,
                directory.mtime,
                directory.cluster,
                directory.attributes,
            );
            let _name_buf: String<BOOK_NAME_MAX> = String::new();
            if let Some(lfn) = lfn {
                // Use lfn directly (already valid UTF-8 from embedded-sdmmc)
                // Skip hidden files (starting with '.')
                if lfn.starts_with('.') {
                    return ControlFlow::Continue(());
                }
                // Check extension case-insensitively without to_uppercase()
                if lfn.len() > BOOK_NAME_MAX {
                    println!("Filename too long, skipping: {}", lfn);
                    return ControlFlow::Continue(());
                }
                if !ends_with_ignore_case(lfn, ".txt") {
                    return ControlFlow::Continue(());
                }
                // Strip extension: find last '.' and take everything before it
                if let Some(dot_pos) = lfn.rfind('.') {
                    let name_part = &lfn[..dot_pos];
                    if let Ok(s) = String::from_str(name_part) {
                        let _ = books.push(s);
                    }
                }
            } else {
                if directory.name.extension() == b"TXT"{
                    let name = String::from_utf8(Vec::try_from(directory.name.base_name()).unwrap()).unwrap();
                   let _ = books.push(name);
                }
            }
            ControlFlow::Continue(())
        }).unwrap();

        Ok(books)

    }

    pub fn get_images(images_dir:&mut ActualDirectory)->Result<Vec<String<BOOK_NAME_MAX>,40>,SdError>
    {
        let mut images:Vec<String<BOOK_NAME_MAX>,40> = Vec::new();

        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        images_dir.iterate_dir_lfn(&mut lfn_buffer,|directory, lfn|{
            if let Some(lfn) = lfn {
                if lfn.starts_with('.') { return ControlFlow::Continue(()); }
                if lfn.len() > BOOK_NAME_MAX { return ControlFlow::Continue(()); }
                if !ends_with_ignore_case(lfn, ".bmp") { return ControlFlow::Continue(()); }
                if let Some(dot_pos) = lfn.rfind('.') {
                    let name_part = &lfn[..dot_pos];
                    if let Ok(s) = String::from_str(name_part) {
                        let _ = images.push(s);
                    }
                }
            } else {
                if directory.name.extension() == b"BMP" {
                    let name = String::from_utf8(Vec::try_from(directory.name.base_name()).unwrap()).unwrap();
                    let _ = images.push(name);
                }
            }
            ControlFlow::Continue(())
        }).unwrap();

        Ok(images)
    }

    pub fn find_entry_by_name(books_dir: &mut ActualDirectory, file_name: &str) -> Option<embedded_sdmmc::DirEntry> {
        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        let mut target_entry = None;

        let _ = books_dir.iterate_dir_lfn(&mut lfn_buffer, |directory, lfn| {
            let matched = if let Some(lfn_name) = lfn {
                eq_ignore_case(lfn_name, file_name)
            } else {
                let full_name = directory.name.to_string();
                full_name == file_name.to_uppercase()
            };
            if matched {
                target_entry = Some(directory.clone());
            }
            ControlFlow::Continue(())
        });

        target_entry
    }

    /// Derive a short filename by replacing the extension of a ShortFileName.
    /// e.g. "MYBOOK~1.TXT" with new_ext "IDX" -> "MYBOOK~1.IDX"
    pub fn derive_short_name(entry_name: &ShortFileName, new_ext: &str) -> Option<ShortFileName> {
        let original = entry_name.to_string();
        let base = if let Some(dot_pos) = original.rfind('.') {
            &original[..dot_pos]
        } else {
            &original
        };
        let new_name = format!("{}.{}", base, new_ext);
        ShortFileName::create_from_str(&new_name).ok()
    }

    /// Open a file by its long file name for reading.
    /// Returns (file, short_name) where short_name can be used to derive .idx/.log names.
    pub fn open_txt_file<'a>(books_dir: &'a mut ActualDirectory, book_name: &str) -> Result<(ActualFile<'a>, ShortFileName), SdError> {
        let file_name = format!("{}.txt", book_name);
        if let Some(entry) = Self::find_entry_by_name(books_dir, &file_name) {
            let short_name = entry.name.clone();
            if let Ok(file) = books_dir.open_file_in_dir(entry.name, Mode::ReadOnly) {
                return Ok((file, short_name));
            }
        }
        Err(FileNotFound)
    }

    /// Open or create the index file (.idx) for a book, using the book's short name.
    pub fn open_idx_file<'a>(books_dir: &'a mut ActualDirectory, book_short_name: &ShortFileName, mode: Mode) -> Result<ActualFile<'a>, SdError> {
        let idx_name = Self::derive_short_name(book_short_name, "IDX").ok_or(FileNotFound)?;
        books_dir.open_file_in_dir(idx_name, mode).map_err(|_| FileNotFound)
    }

    /// Delete the index file for a book.
    pub fn delete_idx_file(books_dir: &mut ActualDirectory, book_short_name: &ShortFileName) -> Result<(), SdError> {
        let idx_name = Self::derive_short_name(book_short_name, "IDX").ok_or(FileNotFound)?;
        books_dir.delete_entry_in_dir(idx_name).map_err(|_| FileNotFound)
    }

    /// Check if the index file exists for a book.
    pub fn idx_file_exists(books_dir: &mut ActualDirectory, book_short_name: &ShortFileName) -> bool {
        let idx_name = match Self::derive_short_name(book_short_name, "IDX") {
            Some(n) => n,
            None => return false,
        };
        books_dir.find_directory_entry(idx_name).is_ok()
    }

    /// Open or create the log file (.log) for a book, using the book's short name.
    pub fn open_log_file<'a>(books_dir: &'a mut ActualDirectory, book_short_name: &ShortFileName, mode: Mode) -> Result<ActualFile<'a>, SdError> {
        let log_name = Self::derive_short_name(book_short_name, "LOG").ok_or(FileNotFound)?;
        books_dir.open_file_in_dir(log_name, mode).map_err(|_| FileNotFound)
    }

    pub fn open_file_by_name<'a>(books_dir: &'a mut ActualDirectory, file_name: &str, mode: embedded_sdmmc::Mode) -> Result<ActualFile<'a>, SdError> {
        // 0.10.0: 优先用长文件名直接打开（内部一次遍历），失败则按短名回退
        if let Ok(f) = books_dir.open_long_name_file_in_dir(file_name, mode) {
            return Ok(f);
        }
        // 短名回退（用于 .LOG/.IDX 等无 LFN 的文件，或创建模式）
        if let Ok(f) = books_dir.open_file_in_dir(file_name, mode) {
            return Ok(f);
        }
        Err(FileNotFound)
    }

}
