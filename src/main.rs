#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![feature(round_char_boundary)]
#![allow(static_mut_refs)]

extern crate alloc;
use alloc::{format, vec};
use core::{borrow::BorrowMut, cell::RefCell};
use esp_hal::riscv::_export::critical_section::Mutex;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, Text, TextStyleBuilder};
use embedded_sdmmc::{sdcard::AcquireOpts, SdCard, VolumeManager};
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    peripherals::Peripherals,
    prelude::*,
    system::SystemControl,
    timer::{timg::TimerGroup, ErasedTimer, OneShotTimer},
    spi::*,
};
use esp_println::{logger::init_logger, print, println};

use esp_hal::prelude::{_fugit_RateExtU32, main};
use esp_hal::{Cpu, dma_descriptors, entry};
use esp_hal::delay::Delay;
use esp_hal::dma::{Dma, DmaPriority};
use esp_hal::gpio::{Input, Io, Output, Pull};
use esp_hal::peripheral::Peripheral;
use esp_hal::spi::master::Spi;
use esp_hal::spi::SpiMode;

use embedded_hal::spi::*;
use esp_hal::spi::master::*;
use embedded_hal_bus::spi::{CriticalSectionDevice, ExclusiveDevice};
use epd_waveshare::color::{Black, White};
use epd_waveshare::epd2in9::{Display2in9, Epd2in9};
use epd_waveshare::prelude::{Display, WaveshareDisplay};
use heapless::{String, Vec};
use log::{debug, error, trace};
use reqwless::request::RequestBody;
use core::str::FromStr;
use embedded_layout::View;

#[macro_export]
macro_rules! make_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

// This is just a placeholder TimeSource. In a real world application
// one would probably use the RTC to provide time.
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

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    alloc();
    println!("entry");
    let mut peripherals = Peripherals::take();

    let mut system = SystemControl::new(unsafe{peripherals.SYSTEM.clone_unchecked()});

    let clocks = ClockControl::max(system.clock_control).freeze();
    let systimer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);

    let timg0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    esp_hal_embassy::init(&clocks, timg0.timer0);

    spawner.spawn(main_loop()).ok();

    let mut io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    let epd_busy = Input::new(io.pins.gpio6,Pull::Down);
    let epd_rst =  Output::new(io.pins.gpio7,esp_hal::gpio::Level::High);
    let epd_dc = Output::new (io.pins.gpio5,esp_hal::gpio::Level::High);
    let epd_cs = Output::new(io.pins.gpio1,esp_hal::gpio::Level::High );
    let epd_sclk = io.pins.gpio2;
    let epd_mosi = io.pins.gpio3;
    let epd_miso = io.pins.gpio10;
    let epd_cs_ph = Output::new(io.pins.gpio13,esp_hal::gpio::Level::High);

    let sdcard_cs = Output::new(io.pins.gpio0,esp_hal::gpio::Level::High );

    let spi = Spi::new(peripherals.SPI2, 400_u32.kHz(), SpiMode::Mode0, &clocks)
        .with_sck(epd_sclk)
        .with_miso(epd_miso)
        .with_mosi(epd_mosi);
    let mut delay = Delay::new(&clocks);

    //spi.change_bus_frequency(40_u32.kHz(), &clocks); 


    let mut_spi = Mutex::new(RefCell::new(spi));
    

    //let mut spi_bus = ExclusiveDevice::new(spi, epd_cs, delay);
    let mut spi_bus = CriticalSectionDevice::new(&mut_spi,sdcard_cs,delay);
    let mut spi_bus_2 = CriticalSectionDevice::new(&mut_spi,epd_cs,delay);


    //init_logger(log::LevelFilter::Trace);
    trace!("test trace");


    let sdcard = SdCard::new_with_options(&mut spi_bus,  delay,AcquireOpts{use_crc:false,acquire_retries:50});

    let mut volume_mgr = VolumeManager::new(sdcard,TimeSource);



    match volume_mgr.device().num_bytes() {
        Ok(size) =>{
            println!("card size is {} bytes", size);

        },
        Err(e) => {
            println!("Error retrieving card size: {:?}", e);

        }
    }


    let mut volume0 = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0));
    match volume0 {
        Ok(mut v) => {
            match v.open_root_dir() {
                Ok(mut root) => {
                    println!("open finish");
                    let mut my_file = root.open_file_in_dir("reader.txt", embedded_sdmmc::Mode::ReadOnly).unwrap();

                    let mut utf8_buf:Vec<u8,600> = Vec::new();//完整的utf8 缓存
                    let mut txt_str:String<8000> = String::new();//保存utf8转换的字符串，大于一定长度后进行分页计算
                    let mut begin_position = 0;//txt_str开始字节在文件中的位置
                    let mut end_position = 0;//txt_str结束字节在文件中的位置
                    let mut all_page_position_vec:Vec<u16,500> = Vec::new();

                    const BEGIN_PAGE_LEN:usize = 7000;

                    const buffer_len:usize = 500;

                    let mut file_length = my_file.length();
                    println!("文件大小：{}", file_length);

                    while !my_file.is_eof() {
                        let mut buffer = [0u8; buffer_len];
                        let num_read = my_file.read(&mut buffer).unwrap();
                        debug!("buffer num:{}",num_read);
                        debug!("buffer : {:?}",buffer );

                        let mut cut_buffer = cut_full_utf8(&buffer,num_read,buffer_len);
                        for b in &buffer[0..cut_buffer.len()] {
                            utf8_buf.push(*b).unwrap();
                        }

                        debug!("cut_buffer : {:?}",cut_buffer );

                        end_position += utf8_buf.as_slice().len();
                        // 检查当前缓冲区中的字节是否形成了有效的UTF-8字符
                        if let Ok(s) = String::from_utf8(utf8_buf.clone()) {
                            txt_str.push_str(s.as_str());
                            // 有效的UTF-8字符，可以打印或处理
                            debug!("read 字符：{}", s);
                            debug!("字符：{}", txt_str);
                            utf8_buf.clear(); // 清空缓冲区，准备下一批字节
                        } else {
                            debug!("Invalid UTF-8 sequence");
                            utf8_buf.clear();
                        }
                        if cut_buffer.len() != num_read {
                            for b in &buffer[cut_buffer.len()..num_read] {
                                utf8_buf.push(*b);
                            }
                        }

                        if(txt_str.len() > BEGIN_PAGE_LEN){
                            let (lost_str,pages) =  compute_pages(txt_str.as_str(),begin_position);

                            //结束位置减掉剩余的长度是新的开始位置，剩余的字符串会重新加入到txt_str开始位置
                            begin_position = end_position - lost_str.len();
                            txt_str =String::from_str(lost_str).expect("lost_str error");

                            //计算进度
                            let percent =  begin_position as f32 / file_length as f32 * 100.0;
                            println!("完成：{}%", percent);


                            all_page_position_vec.extend_from_slice(&pages);

                        }

                    }

                    //结束时最后计算
                    let (lost_str,pages) =  compute_pages(txt_str.as_str(),begin_position);
                    all_page_position_vec.extend_from_slice(&pages);

                    debug!("txt_str:{}",txt_str);
                    debug!("pages:{:?}",all_page_position_vec);


                    for i in 0..all_page_position_vec.len() {
                        let (start_position,end_position) = get_page_content(i+1,&all_page_position_vec);

                        my_file.seek_from_start(start_position as u32);

                        let mut buffer = [0u8; BEGIN_PAGE_LEN];
                        let num_read = my_file.read(&mut buffer).unwrap();

                        let len = end_position - start_position ;
                        let len = len as usize;
                        let vec:Vec<u8,500> = Vec::from_slice(&buffer[0..len]).expect("REASON");
                        if let Ok(screen_txt) = String::from_utf8(vec) {
                            println!("page : {} screen_txt:{}",(i+1),screen_txt);
                        }

                    }




                },
                Err(er) => {
                    println!("open volume:{:?}",er);
                },
            }
        },
        Err(e) => {
            println!("open volume:{:?}",e);
        }
    }



    println!("read end");

    /*let mut epd = Epd2in9::new(&mut spi_bus_2, epd_cs_ph , epd_busy, epd_dc, epd_rst, &mut delay).unwrap();

    let mut display: Display2in9 = Display2in9::default();
    draw_text(&mut display, "hello world!", 5, 50);
    epd.update_and_display_frame(&mut spi_bus_2,display.buffer(),&mut delay );
    epd.sleep(&mut spi_bus_2, &mut delay);*/

    println!("render end");
    loop{
        Timer::after_secs(1).await;
    }
}


//从buffer 中找到utf8可以完整结束的位置并返回
fn cut_full_utf8(buffer:&[u8],len:usize,full_len:usize)->&[u8]{
    if len < full_len{
        return &buffer[0..len];
    }else {
        let mut tail_position = len-1;

        while   tail_position > 0{
            let last_byte = buffer[tail_position];

            //首位为0 ，ascii
            if last_byte & 0b1000_0000 == 0 {
                return &buffer[0..=tail_position];
            }
            //是否为字符第一个byte，0b10开头不是第一个byte
            if last_byte & 0b1100_0000 == 0b1000_0000  {
                tail_position -= 1;
            }else{
                break;
            }
        }
        if tail_position < 0 {
            return &buffer[0..=0usize];
        }

        &buffer[0..tail_position]
    }
}

fn compute_pages(txt_str:&str,begin_position:usize)->(&str,Vec<u16,50>){

    //position 是对应文件中的下标
    let mut real_position = begin_position as u16;
    let mut page_positions:Vec<u16,50> = Vec::new();


    //index 对应切片的下标
    let mut begin_index:usize = 0;
    while begin_index  < txt_str.len()  {
        let (screen_str, is_full_screen) = compute_page(&txt_str[begin_index..]);

        real_position = real_position + screen_str.len() as u16;
        begin_index = begin_index +  screen_str.len() ;
        page_positions.push(real_position).expect("compute_pages error");

        if !is_full_screen {
            break ;
        }
    }


    (&txt_str[begin_index as usize..],page_positions)


}

//计算整屏的文本，返回字符串切片，及是否为完整一屏
fn compute_page(txt_str:&str)->(&str,bool){
    const LOW_WORD:usize = 300;//起步的字符数量
    if txt_str.len() > LOW_WORD {

        let mut end = txt_str.ceil_char_boundary(LOW_WORD);

        let mut is_full_screen = true;
        //循环判断
        while  end < txt_str.len() {
            if check_full_screen(&txt_str[0..end]) {
                is_full_screen = true;
                break;
            }else{
                is_full_screen = false;
            }
            end+=1;
        }
        (&txt_str[0..end],is_full_screen)
    }else{
        (txt_str,false)
    }
}
fn check_full_screen(txt_str:&str)->bool{
    true
}
//从1开始
fn get_page_content( page_num:usize,pages_vec:&Vec<u16,500>)-> (u16,u16){


    let mut  start_position = 0;
    let mut  end_position = 0;

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
    (start_position,end_position)
}

#[embassy_executor::task]
async fn main_loop(){
    loop {
        println!("main_loop");
        Timer::after_secs(5).await;
    }
}

fn alloc(){
    // -------- Setup Allocator --------
    const HEAP_SIZE: usize = 60 * 1024;
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    #[global_allocator]
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe { ALLOCATOR.init(&mut HEAP as *const u8 as usize, core::mem::size_of_val(&HEAP)) };
}
fn draw_text(display: &mut Display2in9, text: &str, x: i32, y: i32) {
    let style = MonoTextStyleBuilder::new()
        .font(&embedded_graphics::mono_font::ascii::FONT_6X10)
        .text_color(White)
        .background_color(Black)
        .build();

    let text_style = TextStyleBuilder::new().baseline(Baseline::Top).build();

    let _ = Text::with_text_style(text, Point::new(x, y), style, text_style).draw(display);
}