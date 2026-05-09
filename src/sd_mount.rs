use alloc::string::ToString;
use alloc::format;
use core::str::FromStr;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{Block, BlockCount, BlockIdx, Directory, Error, File, LfnBuffer, Mode, SdCard, ShortFileName, Volume, VolumeManager};
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
            let mut name_buf: String<BOOK_NAME_MAX> = String::new();
            if let Some(lfn) = lfn {
                // Use lfn directly (already valid UTF-8 from embedded-sdmmc)
                // Skip hidden files (starting with '.')
                if lfn.starts_with('.') {
                    return;
                }
                // Check extension case-insensitively without to_uppercase()
                if lfn.len() > BOOK_NAME_MAX {
                    println!("Filename too long, skipping: {}", lfn);
                    return;
                }
                if !ends_with_ignore_case(lfn, ".txt") {
                    return;
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

        }).unwrap();

        Ok(books)

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
        books_dir.delete_file_in_dir(idx_name).map_err(|_| FileNotFound)
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
        if let Some(entry) = Self::find_entry_by_name(books_dir, file_name) {
            println!("open_file_by_name found entry:{}", entry.name);
            if let Ok(temp) =  books_dir.open_file_in_dir(entry.name,mode) {
                return Ok(temp);
            }else{
                println!("open_file_in_dir failed for entry");
                Err(SdError::FileNotFound)
            }
        } else {
            println!("open_file_by_name entry not found: {}", file_name);
            match mode {
                Mode::ReadWriteCreate |
                Mode::ReadWriteCreateOrTruncate |
                Mode::ReadWriteCreateOrAppend => {
                    if let Ok(temp) =  books_dir.open_file_in_dir(file_name,mode) {
                        Ok(temp)
                    }else{
                        println!("open_file_in_dir fallback failed");
                        Err(FileNotFound)
                    }
                }
                _ =>  Err(FileNotFound),
            }
        }
    }

    /// Calculate LFN checksum from a ShortFileName (FAT32 spec algorithm).
    /// Reconstructs the 11-byte on-disk representation from the string form.
    fn lfn_checksum(sfn: &ShortFileName) -> u8 {
        let s = sfn.to_string().to_uppercase();
        let mut buf = [b' '; 11];
        if let Some(dot) = s.find('.') {
            let base = &s[..dot];
            let ext = &s[dot + 1..];
            for (i, &b) in base.as_bytes().iter().enumerate() {
                if i >= 8 { break; }
                buf[i] = b;
            }
            for (i, &b) in ext.as_bytes().iter().enumerate() {
                if i >= 3 { break; }
                buf[8 + i] = b;
            }
        } else {
            for (i, &b) in s.as_bytes().iter().enumerate() {
                if i >= 8 { break; }
                buf[i] = b;
            }
        }
        let mut sum: u8 = 0;
        for &b in buf.iter() {
            sum = sum.rotate_right(1).wrapping_add(b);
        }
        sum
    }

    /// Build a single 32-byte LFN directory entry.
    /// seq: sequence number (1-based, bit 6 set for last entry)
    /// checksum: checksum of the associated short name
    /// chars: up to 13 UTF-16LE characters for this entry
    fn build_lfn_entry(seq: u8, checksum: u8, chars: &[u16]) -> [u8; 32] {
        let mut entry = [0u8; 32];
        entry[0] = seq;
        entry[11] = 0x0F; // LFN attribute
        entry[12] = 0x00; // type
        entry[13] = checksum;
        // Bytes 26-27: first cluster (always 0 for LFN)
        entry[26] = 0x00;
        entry[27] = 0x00;

        // Characters 1-5: bytes 1-10
        for i in 0..5 {
            let c = chars.get(i).copied().unwrap_or(if i < chars.len() + 1 { 0xFFFF } else { 0x0000 });
            // If within name length, use char; at name length, use 0x0000; beyond, use 0xFFFF
            let val = if i < chars.len() {
                chars[i]
            } else if i == chars.len() {
                0x0000 // null terminator
            } else {
                0xFFFF // padding
            };
            entry[1 + i * 2] = (val & 0xFF) as u8;
            entry[2 + i * 2] = (val >> 8) as u8;
        }
        // Characters 6-11: bytes 14-25
        for i in 0..6 {
            let idx = 5 + i;
            let val = if idx < chars.len() {
                chars[idx]
            } else if idx == chars.len() {
                0x0000
            } else {
                0xFFFF
            };
            entry[14 + i * 2] = (val & 0xFF) as u8;
            entry[15 + i * 2] = (val >> 8) as u8;
        }
        // Characters 12-13: bytes 28-31
        for i in 0..2 {
            let idx = 11 + i;
            let val = if idx < chars.len() {
                chars[idx]
            } else if idx == chars.len() {
                0x0000
            } else {
                0xFFFF
            };
            entry[28 + i * 2] = (val & 0xFF) as u8;
            entry[29 + i * 2] = (val >> 8) as u8;
        }

        entry
    }

    /// Generate a unique 8.3 short name for a new file.
    /// Scans the directory for existing BKxxxx~y.TXT entries.
    fn generate_unique_short_name(books_dir: &mut ActualDirectory) -> Result<ShortFileName, SdError> {
        let mut max_num: u32 = 0;
        let mut storage = [0u8; 512];
        let mut lfn_buffer = LfnBuffer::new(&mut storage);
        let _ = books_dir.iterate_dir_lfn(&mut lfn_buffer, |dir_entry, _lfn| {
            let name_str = dir_entry.name.to_string();
            // Check for BKxxxx~n pattern
            if name_str.starts_with("BK") && name_str.contains('~') {
                // Extract number before ~
                if let Some(tilde_pos) = name_str.find('~') {
                    let num_str = &name_str[2..tilde_pos];
                    if let Ok(n) = num_str.parse::<u32>() {
                        if n > max_num {
                            max_num = n;
                        }
                    }
                }
            }
        });
        let next_num = max_num + 1;
        let short = format!("BK{:04X}~1.TXT", next_num);
        ShortFileName::create_from_str(&short).map_err(|_| FileNotFound)
    }

    /// Create a new file with a long filename (LFN support).
    /// 1. Creates file with auto-generated short name (handles FAT/cluster allocation)
    /// 2. Inserts LFN entries into the directory before the short name entry
    /// Returns (short_name, dir_entry_block, dir_entry_offset)
    pub fn create_file_with_lfn(
        volume_mgr: &mut ActualVolumeManager,
        long_name: &str,
    ) -> Result<(ShortFileName, embedded_sdmmc::BlockIdx, u32), SdError> {
        // Phase 1: Generate unique short name
        let short_name = {
            let mut vol = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0)).map_err(|_| OpenVolumeError)?;
            let mut root = vol.open_root_dir().map_err(|_| OpenRootError)?;
            let mut books_dir = root.open_dir("books").map_err(|_| OpenBooksError)?;
            Self::generate_unique_short_name(&mut books_dir)?
        };

        // Phase 2: Create file with short name
        {
            let mut vol = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0)).map_err(|_| OpenVolumeError)?;
            let mut root = vol.open_root_dir().map_err(|_| OpenRootError)?;
            let mut books_dir = root.open_dir("books").map_err(|_| OpenBooksError)?;
            let file = books_dir.open_file_in_dir(short_name.clone(), Mode::ReadWriteCreateOrTruncate)
                .map_err(|_| FileNotFound)?;
            file.close();
        }

        // Phase 3: Get entry position
        let dir_entry = {
            let mut vol = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0)).map_err(|_| OpenVolumeError)?;
            let mut root = vol.open_root_dir().map_err(|_| OpenRootError)?;
            let mut books_dir = root.open_dir("books").map_err(|_| OpenBooksError)?;
            books_dir.find_directory_entry(short_name.clone())
                .map_err(|e| {
                    println!("find_directory_entry error: {:?}", e);
                    FileNotFound
                })?
        };

        let entry_block = dir_entry.entry_block;
        let entry_offset = dir_entry.entry_offset;
        println!("LFN: entry_block={:?} offset={} short={}", entry_block, entry_offset, short_name);

        // Phase 4: Add LFN entries via raw block device access
        let checksum = Self::lfn_checksum(&short_name);
        let utf16_chars: alloc::vec::Vec<u16> = long_name.encode_utf16().collect();
        let num_lfn_entries = (utf16_chars.len() + 12) / 13; // ceil(len/13)
        let entry_idx = (entry_offset / 32) as usize;

        // Check if there's enough room in this sector (16 entries per 512-byte sector)
        if entry_idx + num_lfn_entries >= 16 {
            println!("LFN: not enough room in sector for {} entries at idx {}", num_lfn_entries, entry_idx);
            // Still return success - file exists without LFN
            return Ok((short_name, entry_block, entry_offset));
        }

        volume_mgr.device(|block_dev| {
            let mut blocks = [Block::new()];
            match embedded_sdmmc::BlockDevice::read(block_dev, &mut blocks, entry_block) {
                Ok(_) => {
                    let data = &mut blocks[0].contents;
                    let shift_bytes = num_lfn_entries * 32;
                    let src_start = entry_offset as usize;

                    for i in (src_start..512 - shift_bytes).rev() {
                        data[i + shift_bytes] = data[i];
                    }

                    for seq in 0..num_lfn_entries {
                        let lfn_seq = num_lfn_entries - seq; // 1-based sequence (N, N-1, ..., 1)
                        let is_last = seq == 0;
                        let seq_byte = if is_last { 0x40 | (lfn_seq as u8) } else { lfn_seq as u8 };
                        // Characters for sequence lfn_seq: (lfn_seq-1)*13 .. lfn_seq*13
                        let start_char = (lfn_seq as usize - 1) * 13;
                        let end_char = core::cmp::min(lfn_seq as usize * 13, utf16_chars.len());
                        let chunk = &utf16_chars[start_char..end_char];

                        let lfn_entry = Self::build_lfn_entry(seq_byte, checksum, chunk);
                        let offset = src_start + seq * 32;
                        data[offset..offset + 32].copy_from_slice(&lfn_entry);
                    }

                    let _ = embedded_sdmmc::BlockDevice::write(block_dev, &blocks, entry_block);
                }
                Err(e) => {
                    println!("LFN read error: {:?}", e);
                }
            }
            TimeSource
        });

        Ok((short_name, entry_block, entry_offset))
    }
}
