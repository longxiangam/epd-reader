#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![feature(round_char_boundary)]
#![allow(static_mut_refs)]


mod epd2in9_txt;
mod sd_mount;
mod event;
mod sleep;
mod utils;
mod wifi;
mod worldtime;
mod pages;
mod display;
mod widgets;

extern crate alloc;
use alloc::{format, vec};
use core::{borrow::BorrowMut, cell::RefCell};
use esp_hal::riscv::_export::critical_section::{CriticalSection, Mutex};
use esp_hal::riscv::_export::critical_section;
use esp_hal::riscv::_export::critical_section::{with};

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

use esp_hal::gpio::{Input, Io, Output, Pull};
use esp_hal::peripheral::Peripheral;
use esp_hal::spi::master::Spi;
use esp_hal::spi::SpiMode;

use embassy_time::Delay;

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
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
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
use futures::SinkExt;

use esp_hal::clock::Clocks;
use esp_hal::peripherals::SPI2;
use static_cell::StaticCell;
use esp_hal::gpio::{Gpio1,Gpio0};
use alloc::string::ToString;
use crate::sd_mount::{ SdCsPin, SdMount};

pub static mut CLOCKS_REF: Option<&'static Clocks>  =  None;

pub static SHARE_SPI:embassy_sync::mutex::Mutex<CriticalSectionRawMutex,Option<Spi<SPI2,FullDuplexMode>>> = embassy_sync::mutex::Mutex::new(None);

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    alloc();
    println!("entry");
    let mut peripherals = Peripherals::take();

    let mut system = SystemControl::new(unsafe{peripherals.SYSTEM.clone_unchecked()});

    let clocks = ClockControl::max(system.clock_control).freeze();

    let systimer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);

    let timg0 = TimerGroup::new(unsafe{peripherals.TIMG0.clone_unchecked()}, &clocks);
    esp_hal_embassy::init(&clocks, timg0.timer0);
   /* let stack =  crate::wifi::connect_wifi(&spawner,
                                           peripherals.TIMG0,
                                           Rng::new(peripherals.RNG),
                                           peripherals.WIFI,
                                           peripherals.RADIO_CLK,
                                           &clocks).await;
    spawner.spawn(crate::worldtime::ntp_worker()).ok();
    loop {
        if let Some(clock) = worldtime:: get_clock(){
            let local = clock.local().await;
            let hour = local.hour();
            let minute = local.minute();
            let second = local.second();
            let str = format_args!("{:02}:{:02}:{:02}",hour,minute,second).to_string();

            println!("Current_time: {} {}", clock.get_date_str().await,str);
        }
        Timer::after(Duration::from_secs(10)).await;

    }*/

    spawner.spawn(main_loop()).ok();

    let mut io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    let epd_busy = io.pins.gpio6;
    let epd_rst =  io.pins.gpio7;
    let epd_dc = io.pins.gpio5;
    let epd_cs = Output::new(io.pins.gpio1,esp_hal::gpio::Level::High );
    let epd_sclk = io.pins.gpio2;
    let epd_mosi = io.pins.gpio3;
    let epd_miso = io.pins.gpio10;
    let epd_cs_ph = Output::new(io.pins.gpio13,esp_hal::gpio::Level::High);

    let sdcard_cs = Output::new(io.pins.gpio0,esp_hal::gpio::Level::High );

    let spi = Spi::new(peripherals.SPI2, 10000_u32.kHz(), SpiMode::Mode0, &clocks)
        .with_sck(epd_sclk)
        .with_miso(epd_miso)
        .with_mosi(epd_mosi);
    //let mut delay = Delay::new(&clocks);

    //spi.change_bus_frequency(40_u32.kHz(), &clocks); 
    let mut key1 =  io.pins.gpio20;
    let key2 = io.pins.gpio8;
    let key3 =  io.pins.gpio9;
    spawner.spawn(event::run(key1,key2,key3)).ok();

    let mut_spi = Mutex::new(RefCell::new(spi));
    let mut_spi_static = make_static!( Mutex<RefCell<Spi<SPI2, FullDuplexMode>>>,mut_spi);

    //let mut spi_bus = ExclusiveDevice::new(spi, epd_cs, delay);
    let mut spi_bus_sd = CriticalSectionDevice::new(mut_spi_static,sdcard_cs,Delay);
    let mut spi_bus_epd = CriticalSectionDevice::new(mut_spi_static,epd_cs,Delay);

    let spi_bus_sd = make_static!(CriticalSectionDevice<Spi<SPI2, FullDuplexMode>, Output<Gpio0>, Delay>,spi_bus_sd);
    let spi_bus_epd = make_static!(CriticalSectionDevice<Spi<SPI2, FullDuplexMode>, Output<Gpio1>, Delay>,spi_bus_epd);
    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
    let mut font = font.with_ignore_unknown_chars(true);
    //init_logger(log::LevelFilter::Trace);
    trace!("test trace");
    spawner.spawn(display::render(spi_bus_epd,epd_busy,epd_rst,epd_dc)).ok();

    let mut display: Display2in9 = Display2in9::default();
    use embedded_graphics::draw_target::DrawTarget;


    display.set_rotation(DisplayRotation::Rotate90);
    use crate::sd_mount::ActualVolumeManager;
    use crate::sd_mount::SdCsPin;

    let sdcard = SdCard::new_with_options(spi_bus_sd,  Delay,AcquireOpts{use_crc:false,acquire_retries:50});

    let mut volume_mgr = VolumeManager::new(sdcard,crate::sd_mount:: TimeSource);

    let mut sd_mount = SdMount::new(volume_mgr);
    crate::sd_mount::SD_MOUNT.lock().await.replace(sd_mount);




/*    let mut my_struct = MyStruct{open_volume:None};

    let mut a = my_struct.get_open_volume();*/



    spawner.spawn(pages::main_task(spawner.clone())).ok();
    Timer::after_millis(10).await;


    loop{
        Timer::after_secs(1).await;
    }
/*


    let mut volume0 = volume_mgr.open_volume(embedded_sdmmc::VolumeIdx(0));
    match volume0 {
        Ok(mut v) => {
            let root_result = v.open_root_dir();
            match root_result {
                Ok(mut root) => {

                    let begin_secs = Instant::now().as_secs();
                    println!("begin_time:{}",begin_secs);
                    let mut pages_vec = None;
                    let mut logs_vec = None;

                    let mut need_index = false;
                    let mut file_len =  0;

                    let file_name = "abc.txt";
                    let index_name = "abc.idx";
                    let log_name = "abc.log";

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
                                println!("file_name:{}",file_name);
                                pages_vec  = Some(crate::epd2in9_txt::TxtReader::read_pages(&mut mfi));
                                println!("file_name:{}",file_name);
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
                    println!("file_name:{}",file_name);
                    if need_index {
                        {
                            let mut my_file = root.open_file_in_dir(file_name, embedded_sdmmc::Mode::ReadOnly).unwrap();
                            pages_vec = Some(TxtReader::generate_pages(&mut my_file));
                            my_file.close();
                        }

                        //写索引
                        let mut my_file_index = root.open_file_in_dir(index_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);

                        if let Ok(mut mfi) = my_file_index {
                            if let Some(ref p_vec) = pages_vec {
                                crate::epd2in9_txt::TxtReader::save_pages(&mut mfi, p_vec);
                            }
                            mfi.close();
                        }

                    }else{
                        //读日志
                        let mut my_file = root.open_file_in_dir(log_name, embedded_sdmmc::Mode::ReadOnly);
                        if let Ok(mut f) = my_file {
                            logs_vec = Some(TxtReader::read_log(&mut f));
                            f.close();
                        }

                    }

                    println!("file_name:{}",file_name);
                    if let Some(ref p_vec) = pages_vec {

                        let mut current_page:usize =  0;
                        if let Some(lv) = logs_vec {
                            if lv.len() > 0 {
                                current_page = lv[0] as usize;
                            }
                        }

                        loop{
                            {
                                println!("file_name:{}",file_name);
                                let mut my_file = root.open_file_in_dir(file_name, embedded_sdmmc::Mode::ReadOnly).unwrap();
                                let content = TxtReader::get_page_content(&mut my_file, current_page + 1, &p_vec);

                                if current_page % 5 == 0 {

                                } else if current_page % 5 == 1 {

                                }
                                //epd.update_and_display_frame(&mut spi_bus_2, display.buffer(), &mut delay);
                                /*if let Some(display) = display::display_mut() {
                                    display.clear_buffer(Color::White);
                                    let _ = font.render_aligned(
                                        content.as_str(),
                                        Point::new(0, 2),
                                        VerticalPosition::Top,
                                        HorizontalAlignment::Left,
                                        FontColor::Transparent(Black),
                                        display,
                                    );

                                    display::RENDER_CHANNEL.send(display::RenderInfo { time: 0, need_sleep: false }).await;
                                }*/

                                my_file.close();
                            }


                            Timer::after_secs(5).await;
                            //key_boot.wait_for_rising_edge().await;
                            current_page += 1;
                            if current_page == p_vec.len() {
                                current_page = 0;
                            }
                            {
                                let logfile = root.open_file_in_dir(log_name, embedded_sdmmc::Mode::ReadWriteCreateOrTruncate);
                                if let Ok(mut f) = logfile{
                                    epd2in9_txt::TxtReader::save_log(&mut f,current_page as u32,false);
                                    f.close();
                                }
                            }
                        }


                    }
                    //epd.sleep(&mut spi_bus_2, &mut delay);



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
*/


    loop{
        Timer::after_secs(1).await;
    }
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
    const HEAP_SIZE: usize = 10 * 1024;
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