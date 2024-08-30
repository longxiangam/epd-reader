#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![feature(round_char_boundary)]
#![allow(static_mut_refs)]

mod txt_reader;
mod epd2in9_txt;

extern crate alloc;
use alloc::{format, vec};
use core::{borrow::BorrowMut, cell::RefCell};
use esp_hal::riscv::_export::critical_section::Mutex;

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, LineHeight, Text, TextStyleBuilder};
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
use epd_waveshare::color::{Black, Color, White};
use epd_waveshare::epd2in9::{Display2in9, Epd2in9};
use epd_waveshare::prelude::{Display, RefreshLut, WaveshareDisplay};
use heapless::{String, Vec};
use log::{debug, error, trace};
use reqwless::request::RequestBody;
use core::str::FromStr;
use embedded_layout::View;
use crate::epd2in9_txt::TxtReader;
use u8g2_fonts::types::VerticalPosition;
use u8g2_fonts::{Content, FontRenderer};
use u8g2_fonts::U8g2TextStyle;
use u8g2_fonts::fonts;
use u8g2_fonts::types::FontColor;
use u8g2_fonts::types::HorizontalAlignment;

use embedded_graphics::primitives::Rectangle;
use embedded_graphics::prelude::Size;
use embedded_text::TextBox;
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::text::renderer::TextRenderer;
use embedded_hal::delay::DelayNs;
use epd_waveshare::graphics::DisplayRotation;
/*use embedded_text::alignment::{HorizontalAlignment, VerticalAlignment};*/
/*use embedded_text::style::{HeightMode, TextBoxStyle, TextBoxStyleBuilder, VerticalOverdraw};*/

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
    let mut epd = Epd2in9::new(&mut spi_bus_2, epd_cs_ph , epd_busy, epd_dc, epd_rst, &mut delay).unwrap();

    let mut display: Display2in9 = Display2in9::default();
    use embedded_graphics::draw_target::DrawTarget;


    display.set_rotation(DisplayRotation::Rotate90);
    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
    let mut font = font.with_ignore_unknown_chars(true);

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


    for i in 0x32u8..=0x32 {
        let c = char::from(i);
        let mut dims = font.get_rendered_dimensions(c,Point::new(0,0),VerticalPosition::Baseline).unwrap();

        println!("else if ch ==  '{}' {{ {} }}",c,dims.bounding_box.unwrap().size.width);
    }



    let mut key_boot = Input::new(io.pins.gpio9,Pull::Up);
    let mut volume0 = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0));
    match volume0 {
        Ok(mut v) => {
            let root_result = v.open_root_dir();
            match root_result {
                Ok(mut root) => {

                    let begin_secs = Instant::now().as_secs();
                    println!("begin_time:{}",begin_secs);
                    let mut pages_vec = None;

                    let mut need_index = false;
                    let mut file_len =  0;

                    let file_name = "xcb.txt";
                    let index_name = "xcb.idx";

                    {
                        let mut my_file = root.open_file_in_dir(file_name, embedded_sdmmc::Mode::ReadOnly).unwrap();
                        file_len = my_file.length();
                        my_file.close();
                    }
                    println!("file len:{}",file_len);
                    {
                        let mut my_file_index = root.open_file_in_dir(index_name, embedded_sdmmc::Mode::ReadOnly);
                        if let Ok(mut mfi) = my_file_index {
                            println!("idx len:{}",mfi.length());
                            if(mfi.length() == 0){
                                need_index  = true;
                            }else{
                                println!("entry read pages");
                                //读索引
                                pages_vec  = Some(crate::epd2in9_txt::TxtReader::read_pages(&mut mfi));
                                if let Some(ref p_vec) = pages_vec{
                                    if p_vec.len() ==  0  {
                                        need_index  = true;
                                    } else if p_vec[p_vec.len() - 1] != file_len{
                                        println!("end_width :{},{}",p_vec[p_vec.len() - 1],file_len);
                                        need_index  = true;
                                    }
                                }

                            }
                            mfi.close();
                        }else {
                            need_index  = true;
                        }
                    }

                    if need_index {
                        {
                            let mut my_file = root.open_file_in_dir(file_name, embedded_sdmmc::Mode::ReadOnly).unwrap();
                            pages_vec = Some(TxtReader::generate_pages(&mut my_file));
                        }

                        //写索引
                        let mut my_file_index = root.open_file_in_dir(index_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);

                        if let Ok(mut mfi) = my_file_index {
                            if let Some(ref p_vec) = pages_vec {
                                crate::epd2in9_txt::TxtReader::save_pages(&mut mfi, p_vec);
                            }
                        }

                    }


                    if let Some(ref p_vec) = pages_vec {
                        let mut my_file = root.open_file_in_dir(file_name, embedded_sdmmc::Mode::ReadOnly).unwrap();
                        for i in 0..p_vec.len() {
                            let content = TxtReader::get_page_content(&mut my_file, i + 1, &p_vec);
                            display.clear_buffer(Color::White);
                            let _ = font.render_aligned(
                                content.as_str(),
                                Point::new(0, 2),
                                VerticalPosition::Top,
                                HorizontalAlignment::Left,
                                FontColor::Transparent(Black),
                                &mut display,
                            );

                            if i % 5 == 0 {
                                epd.set_lut(&mut spi_bus_2, Some(RefreshLut::Full));
                            } else if i % 5 == 1 {
                                epd.set_lut(&mut spi_bus_2, Some(RefreshLut::Quick));
                            }
                            epd.update_and_display_frame(&mut spi_bus_2, display.buffer(), &mut delay);
                            key_boot.wait_for_rising_edge().await;
                        }
                        my_file.close();
                    }
                    epd.sleep(&mut spi_bus_2, &mut delay);



                    println!("end_time:{}",Instant::now().as_secs());
                    println!("cost_time:{}",Instant::now().as_secs() - begin_secs);
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

    //draw_text(&mut display, "hello world!", 5, 50);

    let style =
        U8g2TextStyle::new(fonts::u8g2_font_wqy12_t_gb2312b, Black);




    let clipping_area = Rectangle::new(Point::new(5, 5)
                                       , Size::new(display.bounding_box().size.width - 10,display.bounding_box().size.width - 10));
    //let mut clipped_display = display.clipped(&clipping_area);


    let str =  "abc\r\n123\n中文\n君不见黄河之水天上来，奔流到海不复回。\n君不见高堂明镜悲白发，朝如青丝暮成雪。\n  明夕玦只觉得那个天雷滚滚，心想我日后的人\n生难道就是拥有一个脑残的愿望然后要和脑残\n们接触还被没有大脑的主角打败？这的人生\n";

    display.clear(White);

    println!("str:{}",str);

    let _ = font.render_aligned(
        str,
        Point::new(5,5),
        VerticalPosition::Top,
        HorizontalAlignment::Left,
        FontColor::Transparent(Black),
        &mut display,
    );
    use u8g2_fonts::Content;

    epd.update_and_display_frame(&mut spi_bus_2,display.buffer(),&mut delay );
    epd.sleep(&mut spi_bus_2, &mut delay);
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
    const HEAP_SIZE: usize = 2 * 1024;
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