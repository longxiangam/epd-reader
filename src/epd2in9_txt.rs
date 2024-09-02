use alloc::vec;
use core::iter::once;
use embassy_time::Instant;
use embedded_graphics::prelude::Point;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{File, SdCard};
use epd_waveshare::epd2in9::Display2in9;
use embassy_time::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;
use esp_println::println;
use heapless::{String, Vec};
use log::debug;
use u8g2_fonts::types::VerticalPosition;
use u8g2_fonts::{Content, FontRenderer};
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts;
use u8g2_fonts::types::FontColor;
use u8g2_fonts::types::HorizontalAlignment;
use crate::epd2in9_txt::CharType::{Ascii, Other, Tail, Zh};
use crate::sd_mount::TimeSource;


const LINES_NUM:u32 = 7;//行数
pub const WIDTH: u32 =296;
pub const HEIGHT: u32 =128;
const BUFFER_LEN: usize = 200;
const PAGES_VEC_MAX:usize = 10_000;
const LOG_VEC_MAX:usize = 100;
pub struct TxtReader;

const ZH_WIDTH:u32 = 16;
type FileObject<'a,'b,CS: esp_hal::gpio::OutputPin> = File<'b,SdCard<&'a mut CriticalSectionDevice<'a,Spi<'a,SPI2, FullDuplexMode>, Output<'a,CS>, Delay>, Delay>, TimeSource, 4, 4, 1>;
impl TxtReader {
    pub fn generate_pages<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS>) ->Vec<u32, PAGES_VEC_MAX>   {



        let mut begin_position :u32= 0; //每一屏在文件中的开始位置
        let mut end_position:u32 = 0; //每一屏在文件中的结束位置
        let mut all_page_position_vec: Vec<u32, PAGES_VEC_MAX> = Vec::new();
        let mut line_width = 0;//当前行宽 用于换行
        let mut lines_num = 0;//当前行数 用于换屏
        let mut last_borrow_chars = 0;//上一次缓存结束时最后一个字符有字节未读取到时，算到上一个分页中，这里需要减去后再开始，

        let last_boundary_index = 0;//最后一次字符边界


        let mut file_length = my_file.length();
        println!("文件大小：{}", file_length);

        let begin_sec = Instant::now().as_secs();
        while !my_file.is_eof() {
            let mut buffer = [0u8; BUFFER_LEN];
            let num_read = my_file.read(&mut buffer).unwrap();
          /*  println!("buffer num:{}",num_read);
            println!("buffer : {:?}",buffer );*/

            let mut i = 0;
            if last_borrow_chars > 0 {
                i += last_borrow_chars;
            }

            while  i < num_read {
                let byte = buffer[i];
                let (char_type,byte_num) = char_type_width(byte);

                match char_type {
                    Ascii => {
                        let char = char::from(byte);
                        if char == '\n' || char == '\r' {
                            //判断当前行是否有数据，无数据则不再增加新行
                            if line_width > 0 {
                                lines_num += 1;
                                line_width = 0;
                            }
                        }else{
                            line_width += ascii_width(char);
                        }
                    }
                    Zh => {
                        line_width += ZH_WIDTH;
                    }
                    Other => {
                        line_width += ZH_WIDTH;
                    }
                    Tail => {
                        //不处理
                    }
                }
                //println!("byte_num:{}",byte_num);
                //步进一个字符的字节数
                if byte_num > 0 {
                    end_position += byte_num as u32;
                    i += byte_num as usize;
                }


                //换行
                if line_width > WIDTH && line_width - WIDTH > 2{
                    lines_num += 1;
                    line_width = 0;
                }

                //换屏 保存分页
                if lines_num == LINES_NUM {
                    all_page_position_vec.push(end_position);

                    //重置下一屏的位置
                    begin_position = end_position;

                    lines_num = 0;
                    line_width = 0;


                    if  Instant::now().as_secs() % 5 == 0 {
                        let percent = (end_position as f32 / file_length as f32) * 100.0;
                        println!("进度：{} %",percent);
                    }

                }
            }
            //记录超出
            if i > num_read {
                last_borrow_chars = i - num_read ;
            }else{
                last_borrow_chars = 0;
            }
        }
        if end_position != begin_position {
            all_page_position_vec.push(end_position);
        }

        println!("pages:{:?}",all_page_position_vec);
        println!("pages len:{}",all_page_position_vec.len());
        return all_page_position_vec;
    }

    pub fn get_page_content<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS>,page_num:usize,pages_vec:&Vec<u32,PAGES_VEC_MAX>)->String<1000>{
        let mut  start_position = 0;
        let mut  end_position = 0;
        let mut line_width = 0;//当前行宽 用于换行
        let mut lines_num = 0;//当前行数 用于换屏


        let page = page_num - 1;

        if page_num <= pages_vec.len() {
            if page == 0 {
                start_position = 0;
                end_position = pages_vec[page];
            }else{
                start_position = pages_vec[page-1];
                end_position = pages_vec[page];
            }
        }

        println!("start:{},end:{}",start_position,end_position);
        my_file.seek_from_start(start_position as u32);

        let mut buffer = [0u8; 1000];
        let num_read = my_file.read(&mut buffer).unwrap();
        let mut txt:Vec<u8,1000> = Vec::new();

        let mut i:usize = 0;
        let len = (end_position - start_position) as usize;
        while i < len{
            let byte = buffer[i];
            let (char_type,byte_num) = char_type_width(byte);

            match char_type {
                Ascii => {
                    let char = char::from(byte);
                    if char == '\n' || char == '\r'  {
                        //判断当前行是否有数据，无数据则不再增加新行
                        if line_width > 0 {
                            lines_num += 1;
                            line_width = 0;
                            txt.push(b'\n');
                        }

                        i+=1;
                        continue;
                    }else{
                        line_width += ascii_width(char);
                    }
                }
                Zh => {
                    line_width += ZH_WIDTH;
                }
                Other => {
                    line_width += ZH_WIDTH;
                }
                Tail => {
                    //不处理
                }
            }

            for j in 0..byte_num {
                txt.push(buffer[i+j as usize]);
            }

            i += byte_num as usize;

            //换行
            if line_width > WIDTH && line_width - WIDTH > 2{
                line_width = 0;
                //txt.push(b'\r');
                txt.push(b'\n');
            }


        }


        String::from_utf8(txt).unwrap()

    }



    pub fn save_pages<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS>,pages_vec:&Vec<u32, PAGES_VEC_MAX> ){
        const LEN:usize = PAGES_VEC_MAX * 4;
        let mut buffer:Vec<u8, LEN> = Vec::new() ;

        for i in 0..pages_vec.len() {
            let value = pages_vec[i];
            buffer.push((value >> 24) as u8);
            buffer.push( (value >> 16) as u8);
            buffer.push((value >> 8) as u8);
            buffer.push( value as u8);
        }

        my_file.write(&buffer);

    }
    pub fn read_pages<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS> )->Vec<u32, PAGES_VEC_MAX> {
        println!(" begin read pages");
        //读索引

        let mut buffer = [0u8; PAGES_VEC_MAX * 4];
        let mut num_read = 0;
        while !my_file.is_eof() {
            num_read = my_file.read(&mut buffer).unwrap();
            println!("times");
        }

        let mut pages_vec = Vec::new();
        for i in (0..num_read).step_by(4) {

            let value = ((buffer[i] as u32) << 24) | ((buffer[i + 1] as u32) << 16) | ((buffer[i + 2] as u32) << 8) | buffer[i + 3] as u32;
            pages_vec.push(value);

        }


        pages_vec
    }

    pub fn save_log<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS>,page:u32,is_favorite:bool)->Vec<u32,LOG_VEC_MAX>{

        let mut log_vec:Vec<u32,LOG_VEC_MAX> = Self::read_log(my_file);

        if is_favorite {
            if !log_vec.contains(&page) && log_vec.len() < LOG_VEC_MAX{
                if(log_vec.len() == 0){
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

        my_file.write(&buffer);
        log_vec
    }
    pub fn read_log<CS: esp_hal::gpio::OutputPin>(my_file: &mut FileObject<CS>)->Vec<u32,LOG_VEC_MAX>{
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
#[derive(Debug)]
enum CharType{
    Ascii,
    Zh,
    Other,
    Tail,
}





//字符类型，ascii ,zh及数量
fn char_type_width(byte:u8) ->(CharType, u8){



    let temp = byte & 0b1111_0000;

    if temp == 0b1111_0000
    {
        (Other,4)
    }
    else if temp == 0b1110_0000
    {
        (Zh,3)
    }
    else
    {
        let temp = byte & 0b1100_0000;
        return if temp == 0b0000_0000 {
            return (Ascii,1);
        }else if temp == 0b1000_0000 {
            (Tail,1)
        }else {
            //两字节
            (Other,2)
        }
    }
}

/**
生成程序
    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312b>();
    for i in 0x21u8..=0x7e {
        let c = char::from(i);
        let mut dims = font.get_rendered_dimensions(c,Point::new(0,0),VerticalPosition::Baseline).unwrap();

        println!("else if ch ==  '{}' {{ {} }}",c,dims.bounding_box.unwrap().size.width);
    }
*/
fn ascii_width(ch:char) -> u32{

    if ch ==  '"' { 3 }
    else if ch ==  '#' { 8 }
    else if ch ==  '$' { 7 }
    else if ch ==  '%' { 9 }
    else if ch ==  '&' { 8 }
    else if ch ==  '\'' { 1 }
    else if ch ==  '(' { 4 }
    else if ch ==  ')' { 4 }
    else if ch ==  '*' { 7 }
    else if ch ==  '+' { 11 }
    else if ch ==  ',' { 2 }
    else if ch ==  '-' { 4 }
    else if ch ==  '.' { 2 }
    else if ch ==  '/' { 6 }
    else if ch ==  '0' { 7 }
    else if ch ==  '1' { 5 }
    else if ch ==  '2' { 7 }
    else if ch ==  '3' { 7 }
    else if ch ==  '4' { 7 }
    else if ch ==  '5' { 7 }
    else if ch ==  '6' { 7 }
    else if ch ==  '7' { 7 }
    else if ch ==  '8' { 7 }
    else if ch ==  '9' { 7 }
    else if ch ==  ':' { 2 }
    else if ch ==  ';' { 2 }
    else if ch ==  '<' { 9 }
    else if ch ==  '=' { 10 }
    else if ch ==  '>' { 9 }
    else if ch ==  '?' { 6 }
    else if ch ==  '@' { 9 }
    else if ch ==  'A' { 9 }
    else if ch ==  'B' { 7 }
    else if ch ==  'C' { 8 }
    else if ch ==  'D' { 8 }
    else if ch ==  'E' { 7 }
    else if ch ==  'F' { 6 }
    else if ch ==  'G' { 8 }
    else if ch ==  'H' { 8 }
    else if ch ==  'I' { 3 }
    else if ch ==  'J' { 4 }
    else if ch ==  'K' { 8 }
    else if ch ==  'L' { 7 }
    else if ch ==  'M' { 9 }
    else if ch ==  'N' { 8 }
    else if ch ==  'O' { 9 }
    else if ch ==  'P' { 7 }
    else if ch ==  'Q' { 9 }
    else if ch ==  'R' { 7 }
    else if ch ==  'S' { 7 }
    else if ch ==  'T' { 9 }
    else if ch ==  'U' { 8 }
    else if ch ==  'V' { 9 }
    else if ch ==  'W' { 11 }
    else if ch ==  'X' { 8 }
    else if ch ==  'Y' { 7 }
    else if ch ==  'Z' { 8 }
    else if ch ==  '[' { 2 }
    else if ch ==  '\\' { 6 }
    else if ch ==  ']' { 2 }
    else if ch ==  '^' { 5 }
    else if ch ==  '_' { 9 }
    else if ch ==  '`' { 2 }
    else if ch ==  'a' { 6 }
    else if ch ==  'b' { 7 }
    else if ch ==  'c' { 6 }
    else if ch ==  'd' { 7 }
    else if ch ==  'e' { 7 }
    else if ch ==  'f' { 5 }
    else if ch ==  'g' { 7 }
    else if ch ==  'h' { 7 }
    else if ch ==  'i' { 2 }
    else if ch ==  'j' { 3 }
    else if ch ==  'k' { 6 }
    else if ch ==  'l' { 1 }
    else if ch ==  'm' { 11 }
    else if ch ==  'n' { 6 }
    else if ch ==  'o' { 7 }
    else if ch ==  'p' { 7 }
    else if ch ==  'q' { 7 }
    else if ch ==  'r' { 5 }
    else if ch ==  's' { 6 }
    else if ch ==  't' { 5 }
    else if ch ==  'u' { 7 }
    else if ch ==  'v' { 6 }
    else if ch ==  'w' { 9 }
    else if ch ==  'x' { 6 }
    else if ch ==  'y' { 6 }
    else if ch ==  'z' { 6 }
    else if ch ==  '{' { 6 }
    else if ch ==  '|' { 1 }
    else if ch ==  '}' { 6 }
    else if ch ==  '~' { 7 }
    else if ch ==  ' ' { ZH_WIDTH/4 }
    else{  0 }

}