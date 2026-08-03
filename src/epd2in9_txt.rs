use esp_println::println;
use heapless::Vec;
use crate::sd_mount::ActualFile;

#[cfg(feature = "epd2in9")]
const LINES_NUM:u32 = 7;//行数
#[cfg(feature = "epd2in9")]
pub const WIDTH: u32 =296;
#[cfg(feature = "epd2in9")]
pub const HEIGHT: u32 =128;

#[cfg(feature = "epd4in2")]
const LINES_NUM:u32 = 22;//行数
#[cfg(feature = "epd4in2")]
pub const WIDTH: u32 = 300;
#[cfg(feature = "epd4in2")]
pub const HEIGHT: u32 = 400;

#[cfg(feature = "epd2in7")]
const LINES_NUM:u32 = 9;//行数
#[cfg(feature = "epd2in7")]
pub const WIDTH: u32 = 264;
#[cfg(feature = "epd2in7")]
pub const HEIGHT: u32 = 176;

pub(crate) const PAGES_VEC_MAX:usize = 1_000;
pub(crate) const LOG_VEC_MAX:usize = 100;

pub const ONE_PAGE_CONTENT_LEN:usize = 2000;

pub struct TxtReader;

type FileObject<'a,'b> = ActualFile<'b>;

impl TxtReader {
    /// 原样写回整个 `.log`（Vec<u32>，大端）。
    /// `.log` 语义：`[0]` = 上次阅读字节偏移，`[1..]` = 书签字节偏移。
    pub fn save_log_raw<'a,'b>(my_file: &mut FileObject<'a,'b>, log_vec:&Vec<u32,LOG_VEC_MAX>){
        const LEN:usize = LOG_VEC_MAX * 4;
        let mut buffer:Vec<u8, LEN> = Vec::new() ;
        for i in 0..log_vec.len() {
            let value = log_vec[i];
            buffer.push((value >> 24) as u8);
            buffer.push( (value >> 16) as u8);
            buffer.push((value >> 8) as u8);
            buffer.push( value as u8);
        }
        let result = my_file.write(&buffer);
        match result {
            Ok(_) => {
                println!("log:{:#?}",buffer);
            }
            Err(e)  => {
                println!("log:{:#?}",e);
            }
        }
    }

    /// 写 `.log`：`is_favorite=true` 追加书签（去重），`is_favorite=false` 更新 `[0]`（上次阅读位置）。
    /// `page` 参数现在为字节偏移（page/u32 名保留以兼容签名）。
    pub fn save_log<'a,'b>(my_file: &mut FileObject<'a,'b>, log_vec:&mut Vec<u32,LOG_VEC_MAX>,page:u32,is_favorite:bool){
        if is_favorite {
            // Only check bookmarks (index 1+), not last read position (index 0)
            let already_bookmarked = log_vec.iter().skip(1).any(|&p| p == page);
            if !already_bookmarked && log_vec.len() < LOG_VEC_MAX{
                if log_vec.len() == 0 {
                    log_vec.push(page);
                }
                log_vec.push(page);
            }
        }else {
            if log_vec.len() == 0 {
                log_vec.push(page);
            }else{
                log_vec[0] = page;
            }
        }
        const LEN:usize = LOG_VEC_MAX * 4;
        let mut buffer:Vec<u8, LEN> = Vec::new() ;

        for i in 0..log_vec.len() {
            let value = log_vec[i];
            buffer.push((value >> 24) as u8);
            buffer.push( (value >> 16) as u8);
            buffer.push((value >> 8) as u8);
            buffer.push( value as u8);
        }

        let result = my_file.write(&buffer);
        match result {
            Ok(_) => {
                println!("log:{:#?}",buffer);
            }
            Err(e)  => {
                println!("log:{:#?}",e);
            }
        }

    }

    /// 读 `.log`（Vec<u32>，大端）。`[0]` = 上次阅读字节偏移，`[1..]` = 书签字节偏移。
    pub fn read_log<'a,'b>(my_file: &mut FileObject<'a,'b>)->Vec<u32,LOG_VEC_MAX>{
        let mut log_vec:Vec<u32,LOG_VEC_MAX> = Vec::new();
        let mut buffer = [0u8; LOG_VEC_MAX * 4];
        let mut num_read = 0;
        while !my_file.is_eof() {
            num_read = my_file.read(&mut buffer).unwrap();
        }
        for i in (0..num_read).step_by(4) {
            let value = ((buffer[i] as u32) << 24) | ((buffer[i + 1] as u32) << 16) | ((buffer[i + 2] as u32) << 8) | buffer[i + 3] as u32;
            log_vec.push(value);
        }

        log_vec
    }
}
