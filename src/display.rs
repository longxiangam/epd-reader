use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embedded_graphics::geometry::Point;

use esp_hal::gpio::{Input, Level, Output, Pull};

use epd_waveshare::color::{Black, Color};
use epd_waveshare::epd2in9::{Display2in9, Epd2in9};
use epd_waveshare::prelude::{Display, RefreshLut, WaveshareDisplay};

use embedded_graphics::prelude::Dimensions;
use esp_println::println;
use esp_hal::spi::master::Spi;
use embedded_hal_bus::spi::CriticalSectionDevice;
use epd_waveshare::epd4in2_v3::{Display4in2, Epd4in2};
use epd_waveshare::epd2in7::{Display2in7, Epd2in7};
use epd_waveshare::prelude::DisplayRotation;
use u8g2_fonts::fonts;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use esp_hal::ram;


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

#[cfg(feature = "epd2in7")]
pub type EpdDisplay = Display2in7;
#[cfg(feature = "epd2in7")]
pub type EpdControl<SPI, BUSY, DC, RST, DELAY> = Epd2in7<SPI, BUSY, DC, RST, DELAY>;


#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES:u32 = 0;

/// Stores the last displayed frame for epd2in7 partial refresh.
/// After wake-up from deep sleep, RAM2 is lost, so we use this buffer
/// as the "old frame" reference in set_base_map_quiet to avoid a full flash.
#[cfg(feature = "epd2in7")]
static mut PREV_BUFFER: [u8; 5808] = [0xFF; 5808];

/// Whether PREV_BUFFER contains valid data (first frame has no previous reference).
#[cfg(feature = "epd2in7")]
static mut PREV_BUFFER_VALID: bool = false;

static mut DISPLAY: Option<EpdDisplay> = None;
static mut SLEEP_RENDERER: Option<fn(&mut EpdDisplay)> = None;

pub fn set_sleep_renderer(renderer: Option<fn(&mut EpdDisplay)>) {
    unsafe { core::ptr::addr_of_mut!(SLEEP_RENDERER).write(renderer); }
}

pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex, RenderInfo, 64> = Channel::new();
pub static QUICKLY_LUT_CHANNEL: Channel<CriticalSectionRawMutex, bool, 64> = Channel::new();

type ActualSpi<'a> = CriticalSectionDevice<'a, Spi<'a, esp_hal::Blocking>, Output<'a>, embedded_hal_bus::spi::NoDelay>;

/// epd2in7: every N renders, do a full refresh (set_base_map) to clear ghosting.
#[cfg(feature = "epd2in7")]
const FORCE_FULL_REFRESH_TIMES:u32 = 20;

/// Other displays use the old fast/full refresh alternation.
#[cfg(not(feature = "epd2in7"))]
const FORCE_FULL_REFRESH_TIMES:u32 = 5;

#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: esp_hal::peripherals::GPIO6<'static>,
    rst: esp_hal::peripherals::GPIO7<'static>,
    dc: esp_hal::peripherals::GPIO20<'static>,
)
{
    let busy = Input::new(busy, esp_hal::gpio::InputConfig::default().with_pull(Pull::Down));
    let rst = Output::new(rst, Level::High, esp_hal::gpio::OutputConfig::default());
    let dc = Output::new(dc, Level::High, esp_hal::gpio::OutputConfig::default());

    let mut epd = EpdControl::new(&mut spi_device, busy, dc, rst, &mut embassy_time::Delay).unwrap();
    let mut display: EpdDisplay = EpdDisplay::default();
    display.clear_buffer(Color::White);

    let receiver = RENDER_CHANNEL.receiver();
    let quickly_lut = QUICKLY_LUT_CHANNEL.receiver();
    unsafe {
        core::ptr::addr_of_mut!(DISPLAY).write(Some(display));
    }

    let mut refresh_lut:RefreshLut = RefreshLut::Quick;
    let mut is_sleep = false;
    if refresh_lut == RefreshLut::Quick {
        spi_device = set_refresh_mode(RefreshLut::Quick, &mut epd, spi_device);
    }

    #[cfg(feature = "epd2in7")]
    let mut need_base_map: bool = true;

    loop {

        let render_sign = receiver.receive();
        let quickly_lut = quickly_lut.receive();
        match select(render_sign, quickly_lut).await {
            Either::First(render_info) => {
                add_render_times();
                println!("begin render");

                if is_sleep {
                    epd.wake_up(&mut spi_device, &mut embassy_time::Delay);
                    is_sleep = false;
                    #[cfg(feature = "epd2in7")]
                    { need_base_map = true; }
                }
                let buffer = unsafe { (*core::ptr::addr_of_mut!(DISPLAY)).as_mut().unwrap().buffer() };
                let _len = buffer.len();

                #[cfg(feature = "epd2in7")]
                {
                    let render_count = get_render_times();
                    let need_force_full = render_count >= FORCE_FULL_REFRESH_TIMES && refresh_lut == RefreshLut::Quick;

                    if refresh_lut == RefreshLut::Quick {
                        if need_force_full {
                            // Periodic full refresh to clear ghosting
                            epd.set_base_map(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
                            need_base_map = false;
                            reset_render_times();
                        } else if need_base_map {
                            // After wake-up or first boot: use quiet transition if we have a previous frame
                            let prev_valid = unsafe { core::ptr::addr_of!(PREV_BUFFER_VALID).read() };
                            if prev_valid {
                                let prev = unsafe { &*core::ptr::addr_of!(PREV_BUFFER) };
                                epd.set_base_map_quiet(&mut spi_device, prev, buffer, &mut embassy_time::Delay).expect("render failed");
                            } else {
                                // No previous frame, must do full flash
                                epd.set_base_map(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
                            }
                            need_base_map = false;
                        } else {
                            // Normal partial refresh
                            epd.update_and_display_frame_partial(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
                        }
                        // Save current frame as reference for next render
                        unsafe {
                            (*core::ptr::addr_of_mut!(PREV_BUFFER)).copy_from_slice(buffer);
                            core::ptr::addr_of_mut!(PREV_BUFFER_VALID).write(true);
                        }
                    } else {
                        epd.update_and_display_frame(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
                    }
                }

                #[cfg(not(feature = "epd2in7"))]
                {
                    let need_force_full = get_render_times() % FORCE_FULL_REFRESH_TIMES == 0 && refresh_lut == RefreshLut::Quick;
                    if need_force_full {
                        spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
                    }
                    epd.update_and_display_frame(&mut spi_device, buffer, &mut embassy_time::Delay).expect("render failed");
                    if need_force_full {
                        spi_device = set_refresh_mode(RefreshLut::Quick, &mut epd, spi_device);
                    }
                }

                if render_info.need_sleep {
                    is_sleep = true;
                    epd.sleep(&mut spi_device, &mut embassy_time::Delay);
                    println!("sleep epd");
                }

                println!("end render");
            },
            Either::Second(v) => {
                if v {
                    refresh_lut = RefreshLut::Quick;
                    spi_device = set_refresh_mode(RefreshLut::Quick, &mut epd, spi_device);
                } else {
                    refresh_lut = RefreshLut::Full;
                    spi_device = set_refresh_mode(RefreshLut::Full, &mut epd, spi_device);
                }
                #[cfg(feature = "epd2in7")]
                { need_base_map = true; }
            },
        }
        Timer::after(Duration::from_millis(50)).await;
    }

}

pub fn add_render_times(){
    unsafe {
        *core::ptr::addr_of_mut!(RENDER_TIMES) += 1;
    }
}

pub fn get_render_times()->u32{
    unsafe {
        *core::ptr::addr_of!(RENDER_TIMES)
    }
}

pub fn reset_render_times(){
    unsafe {
        core::ptr::addr_of_mut!(RENDER_TIMES).write(0);
    }
}

pub fn set_refresh_mode< BUSY, DC, RST > (mode:RefreshLut, epd:&mut EpdControl<&'static mut ActualSpi<'static>, BUSY, DC, RST, embassy_time::Delay>, mut spi_device: &'static mut ActualSpi<'static>)
-> &'static mut ActualSpi<'static>
where BUSY: embedded_hal::digital::InputPin, DC: embedded_hal::digital::OutputPin,  RST: embedded_hal::digital::OutputPin
{
    #[cfg(feature = "epd2in9")]
    epd.set_lut( &mut spi_device, Some(mode));

    #[cfg(feature = "epd4in2")]
    {
        epd.set_refresh(&mut spi_device, &mut embassy_time::Delay, mode).expect("切换刷新模式失败");
    }

    #[cfg(feature = "epd2in7")]
    {
        epd.set_refresh(&mut spi_device, &mut embassy_time::Delay, mode).expect("切换刷新模式失败");
    }

    return spi_device;
}


pub fn display_mut()->Option<&'static mut EpdDisplay>{
    unsafe {
        (*core::ptr::addr_of_mut!(DISPLAY)).as_mut()
    }
}

pub async fn show_error(error:&str, need_clear:bool) {
    embassy_time::Timer::after_secs(1).await;
    if let Some(display) = display_mut() {
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
        let font = font.with_ignore_unknown_chars(true);

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

        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep:true }).await;
        Timer::after_secs(1).await;
    }
}


pub async fn show_sleep() {
    embassy_time::Timer::after_secs(1).await;
    if let Some(display) = display_mut() {
        let renderer = unsafe { *core::ptr::addr_of!(SLEEP_RENDERER) };
        if let Some(r) = renderer {
            r(display);
        } else {
            #[cfg(not(feature = "epd2in7"))]
            display.set_rotation(DisplayRotation::Rotate90);
            display.clear_buffer(Color::White);
            let drawn = crate::flash_sleep::draw_sleep_image(display);
            if !drawn {
                let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
                let font = font.with_ignore_unknown_chars(true);
                let _ = font.render_aligned(
                    "睡眠中",
                    Point::new(
                        display.bounding_box().size.width as i32 / 2,
                        display.bounding_box().size.height as i32 / 2,
                    ),
                    VerticalPosition::Center,
                    HorizontalAlignment::Center,
                    FontColor::Transparent(Black),
                    display,
                );
            }
        }
        RENDER_CHANNEL.send(RenderInfo { time: 0, need_sleep:true }).await;
        Timer::after_secs(1).await;
    }
}
