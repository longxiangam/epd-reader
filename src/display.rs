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

use esp_hal::gpio::{Gpio1, Gpio5, Gpio6, Gpio7, Input, Level, Output, NO_PIN, Pull};
use esp_hal::peripherals::SPI2;

use epd_waveshare::color::{Black, Color, White};
use epd_waveshare::epd2in9::{Display2in9, Epd2in9};
use epd_waveshare::prelude::{Display, RefreshLut, WaveshareDisplay};

use embedded_graphics::{Drawable };
use esp_println::println;
use esp_hal::Async;
use esp_hal::spi::{Error, FullDuplexMode, SpiDataMode, SpiMode};
use embedded_hal_bus::spi::CriticalSectionDevice;
use esp_hal::spi::master::Spi;
use epd_waveshare::prelude::DisplayRotation;
use esp_hal::peripheral::Peripheral;

pub struct RenderInfo{
    pub time:i32,
    pub need_sleep:bool,
}

pub static mut DISPLAY:Option<Display2in9>  = None;

pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex,RenderInfo, 64> = Channel::new();
pub static QUICKLY_LUT_CHANNEL: Channel<CriticalSectionRawMutex,bool, 64> = Channel::new();
#[embassy_executor::task]
pub async  fn render(mut spi_device: &'static mut CriticalSectionDevice<'static,Spi<'static,SPI2, FullDuplexMode>, Output<'static,Gpio1>, Delay> ,
                     mut busy:Gpio6 ,
                           rst:Gpio7,
                           dc: Gpio5)
{
    let ass_cs = Output::new(unsafe{busy.clone_unchecked()},Level::Low);
    let busy = Input::new(busy, Pull::Down);
    let rst = Output::new( rst, Level::High);
    let dc = Output::new( dc, Level::High);

    let mut epd = Epd2in9::new(&mut spi_device, ass_cs , busy, dc, rst, &mut Delay).unwrap();
    let mut display: Display2in9 = Display2in9::default();
    display.set_rotation(DisplayRotation::Rotate90);
    display.clear_buffer(Color::White);

    let receiver = RENDER_CHANNEL.receiver();
    let quickly_lut = QUICKLY_LUT_CHANNEL.receiver();
    unsafe {
        DISPLAY.replace(display);
    }
    let mut render_times = 0;
    let mut refresh_lut:RefreshLut=RefreshLut::Full;

    const FORCE_FULL_REFRESH_TIMES:u32 =  5;
    loop {

        let render_sign = receiver.receive();
        let quickly_lut = quickly_lut.receive();
        match select(render_sign,quickly_lut).await {
            Either::First(render_info) => {
                render_times +=1;
                println!("begin render");
                let buffer = unsafe { DISPLAY.as_mut().unwrap().buffer() };
                let len = buffer.len();
                let mut need_force_full = false;
                if render_times % FORCE_FULL_REFRESH_TIMES == 0 && refresh_lut == RefreshLut::Quick{
                    need_force_full = true;
                    epd.set_lut(&mut spi_device, Some(RefreshLut::Full));
                }
                epd.update_and_display_frame(&mut spi_device, buffer, &mut Delay);

                if need_force_full {
                    epd.set_lut(&mut spi_device, Some(RefreshLut::Quick));
                }

                if render_info.need_sleep {
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
                    epd.set_lut(&mut spi_device, Some(RefreshLut::Quick));
                }else{
                    refresh_lut = RefreshLut::Full;
                    epd.set_lut(&mut spi_device, Some(RefreshLut::Full));
                }
            },
        }
        Timer::after(Duration::from_millis(50)).await;
    }

}



pub fn display_mut()->Option<&'static mut Display2in9>{
    unsafe {
        DISPLAY.as_mut()
    }
}
