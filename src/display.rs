use core::convert::Infallible;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Delay, Duration, TimeoutError, Timer, with_timeout};
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::Point;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::text::{Baseline, Text, TextStyleBuilder};
use embedded_hal_bus::spi::{DeviceError, ExclusiveDevice};
use esp_hal::riscv::_export::critical_section::Mutex;
use core::{borrow::BorrowMut, cell::RefCell};

use esp_hal::gpio::{Gpio1, Gpio5, Gpio6, Gpio7, Input, Level, Output, NO_PIN, Pull, Gpio20, Gpio3};
use esp_hal::peripherals::SPI2;

use epd_waveshare::color::{Black, Color, White};
use epd_waveshare::epd2in9::{Display2in9, Epd2in9};
use epd_waveshare::prelude::{Display, RefreshLut, WaveshareDisplay};

use embedded_graphics::{Drawable };
use embedded_graphics::prelude::Dimensions;
use esp_println::println;
use esp_hal::Async;
use esp_hal::spi::{Error, FullDuplexMode, SpiDataMode, SpiMode};
use embedded_hal_bus::spi::CriticalSectionDevice;
use epd_waveshare::epd4in2_v3::{Display4in2, Epd4in2};
use esp_hal::spi::master::Spi;
use epd_waveshare::prelude::DisplayRotation;
use esp_hal::peripheral::Peripheral;
use u8g2_fonts::fonts;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

pub struct RenderInfo{
    pub time:i32,
    pub need_sleep:bool,
}

#[cfg(feature = "epd2in9")]
pub type EpdDisplay = Display2in9;
#[cfg(feature = "epd2in9")]
pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd2in9<SPI,  BUSY, DC, RST, DELAY>;

#[cfg(feature = "epd4in2")]
pub type EpdDisplay = Display4in2;
#[cfg(feature = "epd4in2")]
pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd4in2<SPI, BUSY, DC, RST, DELAY>;

pub static mut DISPLAY:Option<EpdDisplay>  = None;

pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex,RenderInfo, 64> = Channel::new();
pub static QUICKLY_LUT_CHANNEL: Channel<CriticalSectionRawMutex,bool, 64> = Channel::new();

type ActualSpi = CriticalSectionDevice<'static,Spi<'static,SPI2, FullDuplexMode>, Output<'static,Gpio3>, Delay>;
#[embassy_executor::task]
pub async  fn render(mut spi_device: &'static mut ActualSpi,
                     mut busy:Gpio6 ,
                           rst:Gpio7,
                           dc: Gpio20)
{
    let busy = Input::new(busy, Pull::Down);
    let rst = Output::new( rst, Level::High);
    let dc = Output::new( dc, Level::High);

    let mut epd = EpdControl::new(&mut spi_device,  busy, dc, rst, &mut Delay).unwrap();
    let mut display: EpdDisplay = EpdDisplay::default();
    //display.set_rotation(DisplayRotation::Rotate90);
    display.clear_buffer(Color::White);

    let receiver = RENDER_CHANNEL.receiver();
    let quickly_lut = QUICKLY_LUT_CHANNEL.receiver();
    unsafe {
        DISPLAY.replace(display);
    }
    let mut render_times = 0;
    let mut refresh_lut:RefreshLut=RefreshLut::Full;
    let mut is_sleep = false;

    const FORCE_FULL_REFRESH_TIMES:u32 =  5;
    loop {

        let render_sign = receiver.receive();
        let quickly_lut = quickly_lut.receive();
        match select(render_sign,quickly_lut).await {
            Either::First(render_info) => {
                render_times +=1;
                println!("begin render");

                if is_sleep {
                    //唤醒
                    epd.wake_up(&mut spi_device,&mut Delay);
                    is_sleep = false;
                }

                let buffer = unsafe { DISPLAY.as_mut().unwrap().buffer() };
                let len = buffer.len();
                let mut need_force_full = false;
                if render_times % FORCE_FULL_REFRESH_TIMES == 0 && refresh_lut == RefreshLut::Quick{
                    need_force_full = true;
                    spi_device  = set_refresh_mode(RefreshLut::Full,&mut epd,spi_device);
                }
                epd.update_and_display_frame(&mut spi_device, buffer, &mut Delay);
                if need_force_full {
                    spi_device  = set_refresh_mode(RefreshLut::Quick,&mut epd,spi_device);
                }

                if render_info.need_sleep {
                    is_sleep = true;
                    epd.sleep(&mut spi_device, &mut Delay);
                    println!("sleep epd");
                }

                if(refresh_lut == RefreshLut::Full){
                    render_times = 0;
                }else{
                    render_times += 1;
                }

                println!("end render");
            },
            Either::Second(v) => {
                if v {
                    refresh_lut = RefreshLut::Quick;
                    spi_device  = set_refresh_mode(RefreshLut::Quick,&mut epd,spi_device);
                }else{
                    refresh_lut = RefreshLut::Full;
                    spi_device  = set_refresh_mode(RefreshLut::Full,&mut epd,spi_device);
                }
            },
        }
        Timer::after(Duration::from_millis(50)).await;
    }

}

pub fn set_refresh_mode< BUSY, DC, RST > (mode:RefreshLut,epd:&mut EpdControl<&'static mut ActualSpi, BUSY, DC, RST,Delay>,mut spi_device: &'static mut  ActualSpi)
-> &'static mut ActualSpi
where BUSY: embedded_hal::digital::InputPin, DC: embedded_hal::digital::OutputPin,  RST: embedded_hal::digital::OutputPin
{
    #[cfg(feature = "epd2in9")]
    epd.set_lut( spi_device, Some(mode));

    #[cfg(feature = "epd4in2")]
    {
        epd.set_refresh(&mut spi_device,&mut Delay,mode).expect("切换刷新模式失败");
    }

    return spi_device;
}


pub fn display_mut()->Option<&'static mut EpdDisplay>{
    unsafe {
        DISPLAY.as_mut()
    }
}

pub async fn show_error(error:&str,need_clear:bool) {
    embassy_time::Timer::after_secs(1).await;
    if let Some(display) = display_mut() {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let mut font = font.with_ignore_unknown_chars(true);

        if need_clear {
            display.clear_buffer(Color::White);
        }
        let _ = font.render_aligned(
            error,
            Point::new(display.bounding_box().center().y, display.bounding_box().center().x),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );






        RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        Timer::after_secs(1).await;
    }
}


pub async fn show_sleep() {
    embassy_time::Timer::after_secs(1).await;
    if let Some(display) = display_mut() {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let mut font = font.with_ignore_unknown_chars(true);

        display.clear_buffer(Color::White);
        
        let _ = font.render_aligned(
            "已进入睡眼状态",
            Point::new(display.bounding_box().center().x, display.bounding_box().center().y),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );

        RENDER_CHANNEL.send(RenderInfo { time: 0,need_sleep:true }).await;
        Timer::after_secs(1).await;
    }
}