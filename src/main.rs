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
mod model;
mod weather;
mod request;
mod random;
//mod web_service;
mod battery;

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
use esp_hal::reset::{get_reset_reason, get_wakeup_cause};
use esp_hal::gpio::{DriveStrength, Gpio2, Gpio3, Gpio5, Input, Io, Level, Output, Pull, RtcPin};
use esp_hal::peripheral::Peripheral;
use esp_hal::spi::master::Spi;
use esp_hal::spi::SpiMode;
use esp_hal::rng::Rng;
use embassy_time::Delay;

use embedded_hal::spi::*;
use esp_hal::spi::master::*;
use embedded_hal_bus::spi::{CriticalSectionDevice, ExclusiveDevice};
use epd_waveshare::color::{Black, Color, White};

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
use esp_hal::rtc_cntl::{Rtc, SocResetReason};
use esp_hal::clock::Clocks;
use esp_hal::peripherals::{ADC1, SPI2};
use static_cell::StaticCell;
use esp_hal::gpio::{Gpio1,Gpio0};
use alloc::string::ToString;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::riscv::asm::delay;
use esp_hal::rtc_cntl::sleep::WakeupLevel;
use crate::battery::Battery;
use crate::sd_mount::{SdCsPin, SdMount};
use crate::sleep::{add_rtcio, refresh_active_time, to_sleep, to_sleep_tips};

pub static mut CLOCKS_REF: Option<&'static Clocks>  =  None;

pub static SHARE_SPI:embassy_sync::mutex::Mutex<CriticalSectionRawMutex,Option<Spi<SPI2,FullDuplexMode>>> = embassy_sync::mutex::Mutex::new(None);

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    alloc();
    println!("entry");
    let mut peripherals = Peripherals::take();

    let mut system = SystemControl::new(peripherals.SYSTEM);

    let clocks_val = ClockControl::max(system.clock_control).freeze();
    let clocks  = make_static!(Clocks,clocks_val);

    let mut rtc = Rtc::new(peripherals.LPWR);
    
    crate::sleep:: RTC_MANGE.lock().await.replace(rtc);
    unsafe {
         CLOCKS_REF.replace(clocks);
    }

    use esp_hal::timer::systimer::{SystemTimer, Target};
    let systimer = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER).split::<Target>();
    esp_hal_embassy::init(&clocks, systimer.alarm0);


    //init_logger(log::LevelFilter::Trace);
    trace!("test trace");

    let reason = get_reset_reason().unwrap_or(SocResetReason::ChipPowerOn);
    println!("reset reason: {:?}", reason);
    let wake_reason = get_wakeup_cause();
    println!("wake reason: {:?}", wake_reason);

    spawner.spawn(main_loop()).ok();

 
    let mut io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    //Output::new(io.pins.gpio12,esp_hal::gpio::Level::Low ).set_low();
    //let mut io13 = Output::new(io.pins.gpio13,esp_hal::gpio::Level::Low );
    //io13.set_drive_strength(DriveStrength::I5mA);
    //io13.set_low();

    // let epd_busy = io.pins.gpio6;
    // let epd_rst =  io.pins.gpio7;
    // let epd_dc = io.pins.gpio5;
    // let epd_cs = Output::new(io.pins.gpio1,esp_hal::gpio::Level::High );
    // let epd_sclk = io.pins.gpio2;
    // let epd_mosi = io.pins.gpio3;
    // let epd_miso = io.pins.gpio10;
    // 
    // 
    // let sdcard_cs = Output::new(io.pins.gpio0,esp_hal::gpio::Level::High );
    
    let epd_busy = io.pins.gpio6;
    let epd_rst =  io.pins.gpio7;
    let epd_dc = io.pins.gpio20;
    let epd_cs = Output::new(io.pins.gpio3,esp_hal::gpio::Level::High );
    let epd_sclk = io.pins.gpio8;
    let epd_mosi = io.pins.gpio0;
    let epd_miso = io.pins.gpio10;
    
    io.pins.gpio1.rtcio_pad_hold(true);
    
    let mut eink_pwr_ctrl = Output::new(io.pins.gpio21,Level::High);// 黑水屏电源
    let mut sd_pwr_ctrl = Output::new(io.pins.gpio1,Level::High);// sd 卡电源

    eink_pwr_ctrl.set_low();
    sd_pwr_ctrl.set_low();
    unsafe {

        sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
        sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
    }


  
    
    let sdcard_cs = Output::new(io.pins.gpio5,esp_hal::gpio::Level::High ); 

    let spi = Spi::new(peripherals.SPI2, 32u32.MHz(), SpiMode::Mode0, &clocks)
        .with_sck(epd_sclk)
        .with_miso(epd_miso)
        .with_mosi(epd_mosi);
    //let mut delay = Delay::new(&clocks);

    //spi.change_bus_frequency(40_u32.kHz(), &clocks); 
    let mut key1 =  io.pins.gpio9;
    let mut key2 = io.pins.gpio2;


    let adc_pin = unsafe{ key2.clone_unchecked() };
    let rtc_pin = unsafe{ key2.clone_unchecked() };
    type AdcCal = AdcCalCurve<ADC1>;
    let mut adc1_config = AdcConfig::new();
    let mut adc1_pin =
        adc1_config.enable_pin_with_cal::<_, AdcCal>(adc_pin, Attenuation::Attenuation11dB);
    let bat_adc1_pin =  adc1_config.enable_pin_with_cal::<_, AdcCal>(io.pins.gpio4, Attenuation::Attenuation11dB); 
    
    let adc1 = Adc::new(peripherals.ADC1, adc1_config);
    event::ADC_PER.lock().await.replace(adc1);
    event::ADC_PIN.lock().await.replace(adc1_pin);
    
    //let key3 =  io.pins.gpio11;
    spawner.spawn(event::run(key1,key2)).ok();

    let battery = Battery::new();
    battery::BATTERY.lock().await.replace(battery);
    battery::ADC_PIN.lock().await.replace(bat_adc1_pin);
    spawner.spawn(crate::battery::test_bat_adc()).ok();
    
    let mut_spi = Mutex::new(RefCell::new(spi));
    let mut_spi_static = make_static!( Mutex<RefCell<Spi<SPI2, FullDuplexMode>>>,mut_spi);

    //let mut spi_bus = ExclusiveDevice::new(spi, epd_cs, delay);
    let mut spi_bus_sd = CriticalSectionDevice::new(mut_spi_static,sdcard_cs,Delay);
    let mut spi_bus_epd = CriticalSectionDevice::new(mut_spi_static,epd_cs,Delay);

    let spi_bus_sd = make_static!(CriticalSectionDevice<Spi<SPI2, FullDuplexMode>, Output<Gpio5>, Delay>,spi_bus_sd);
    let spi_bus_epd = make_static!(CriticalSectionDevice<Spi<SPI2, FullDuplexMode>, Output<Gpio3>, Delay>,spi_bus_epd);
    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
    let mut font = font.with_ignore_unknown_chars(true);

    spawner.spawn(display::render(spi_bus_epd,epd_busy,epd_rst,epd_dc)).ok();

    let mut display:display::EpdDisplay = display::EpdDisplay::default();
    use embedded_graphics::draw_target::DrawTarget;


    display.set_rotation(DisplayRotation::Rotate90);
    use crate::sd_mount::ActualVolumeManager;
    use crate::sd_mount::SdCsPin;

    let sdcard = SdCard::new_with_options(spi_bus_sd,  Delay,AcquireOpts{use_crc:false,acquire_retries:50});

    let mut volume_mgr = VolumeManager::new(sdcard,crate::sd_mount:: TimeSource);

    let mut sd_mount = SdMount::new(volume_mgr);
    crate::sd_mount::SD_MOUNT.lock().await.replace(sd_mount);

    refresh_active_time().await;
    spawner.spawn(crate::worldtime::ntp_worker()).ok();
    Timer::after_millis(10).await;
    spawner.spawn(pages::main_task(spawner.clone())).ok();
    spawner.spawn(crate::weather::weather_worker()).ok();



    let rtc_io = make_static!(Gpio2, rtc_pin);
    add_rtcio( rtc_io,  WakeupLevel::Low).await;

    let stack =  crate::wifi::connect_wifi(&spawner,
                                           peripherals.TIMG0,
                                           Rng::new(peripherals.RNG),
                                           peripherals.WIFI,
                                           peripherals.RADIO_CLK,
                                           &clocks).await;
   
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

    }
}

#[embassy_executor::task]
async fn main_loop(){
    loop {
        println!("main_loop");
        to_sleep_tips(Duration::from_secs(0), Duration::from_secs(180),true).await;
        Timer::after_secs(5).await;
    }
}

fn alloc(){
    // -------- Setup Allocator --------
    const HEAP_SIZE: usize = 45 * 1024;
    static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    #[global_allocator]
    static ALLOCATOR: embedded_alloc::Heap = embedded_alloc::Heap::empty();
    unsafe { ALLOCATOR.init(&mut HEAP as *const u8 as usize, core::mem::size_of_val(&HEAP)) };
}
fn draw_text(display: &mut  display::EpdDisplay , text: &str, x: i32, y: i32) {
    let style = MonoTextStyleBuilder::new()
        .font(&embedded_graphics::mono_font::ascii::FONT_6X10)
        .text_color(White)
        .background_color(Black)
        .build();

    let text_style = TextStyleBuilder::new().baseline(Baseline::Top).build();

    let _ = Text::with_text_style(text, Point::new(x, y), style, text_style).draw(display);
}