#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![allow(unused_must_use)]


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
mod storage;
mod panic;
mod web_service;
mod flash_sleep;
mod location;

extern crate alloc;
use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, Text, TextStyleBuilder};
use embedded_sdmmc::{sdcard::AcquireOpts, SdCard, VolumeManager};

use esp_hal::{
    analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation},
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig, RtcPin},
    interrupt::software::SoftwareInterruptControl,
    rng::Rng,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    Config as HalConfig,
    init as hal_init,
};

use esp_hal::spi::Mode;
use esp_hal::timer::timg::TimerGroup;

use esp_println::println;

use embedded_hal_bus::spi::CriticalSectionDevice;
use epd_waveshare::color::{Black, White};

use epd_waveshare::prelude::Display;
use log::trace;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;

use epd_waveshare::graphics::DisplayRotation;
use esp_hal::rtc_cntl::sleep::WakeupLevel;
use alloc::string::ToString;
use crate::battery::Battery;
use crate::sd_mount::{SdMount, SD_MOUNT};
use crate::sleep::{add_rtcio, refresh_active_time, to_sleep_tips};
use crate::wifi::{WifiModel, WIFI_MODEL};

use critical_section::Mutex as CsMutex;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // 请求缓冲已移至 .bss（request.rs 的静态数组），堆无需再容纳那 25KB，
    // 故从 90KB 降到 64KB，把静态内存让给 .bss/栈，避免 SRAM 溢出挤占主栈。
    esp_alloc::heap_allocator!(size: 64 * 1024);

    println!("entry");
    let config = HalConfig::default().with_cpu_clock(CpuClock::max());
    let peripherals = hal_init(config);

    // Timer for embassy runtime
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // RTC
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    crate::sleep::RTC_MANGE.lock().await.replace(rtc);

    // Init storage
    crate::storage::enter_process().await;
    // 加载全刷间隔缓存（render 任务会读取）
    crate::display::reload_full_refresh_times();

    trace!("test trace");

    let reason = esp_hal::system::reset_reason().unwrap_or(esp_hal::rtc_cntl::SocResetReason::ChipPowerOn);
    println!("reset reason: {:?}", reason);
    let wake_reason = esp_hal::system::wakeup_cause();
    println!("wake reason: {:?}", wake_reason);

    // GPIO pins - direct access from peripherals
    let epd_busy = peripherals.GPIO6;
    let epd_rst = peripherals.GPIO7;
    let epd_dc = peripherals.GPIO20;
    let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
    let epd_sclk = peripherals.GPIO8;
    let epd_mosi = peripherals.GPIO0;
    let epd_miso = peripherals.GPIO10;

    // RTC hold on GPIO1
    peripherals.GPIO1.rtcio_pad_hold(true);

    let mut eink_pwr_ctrl = Output::new(peripherals.GPIO21, Level::High, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(peripherals.GPIO1, Level::High, OutputConfig::default());

    eink_pwr_ctrl.set_low();
    sd_pwr_ctrl.set_low();

    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);

    let sdcard_cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());

    // SPI bus
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(32))
            .with_mode(Mode::_0),
    ).unwrap()
    .with_sck(epd_sclk)
    .with_miso(epd_miso)
    .with_mosi(epd_mosi);

    // Keys
    let key1 = peripherals.GPIO9;

    // ADC setup
    let mut adc1_config = AdcConfig::new();
    let adc_pin = unsafe { peripherals.GPIO2.clone_unchecked() };
    let rtc_pin = unsafe { peripherals.GPIO2.clone_unchecked() };
    let key2 = peripherals.GPIO2;
    let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(adc_pin, Attenuation::_11dB);
    let bat_adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(peripherals.GPIO4, Attenuation::_11dB);

    let adc1 = Adc::new(peripherals.ADC1, adc1_config);
    event::ADC_PER.lock().await.replace(adc1);
    event::ADC_PIN.lock().await.replace(adc1_pin);

    spawner.spawn(event::run(key1, key2).unwrap());

    let battery = Battery::new();
    battery::BATTERY.lock().await.replace(battery);
    battery::ADC_PIN.lock().await.replace(bat_adc1_pin);

    // Shared SPI bus using critical section
    let shared_spi = CsMutex::new(RefCell::new(spi));
    let shared_spi_static = static_cell::make_static!(shared_spi);

    let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
    let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();

    let spi_bus_sd = static_cell::make_static!(spi_bus_sd);
    let spi_bus_epd = static_cell::make_static!(spi_bus_epd);

    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy15_t_gb2312>();
    let _font = font.with_ignore_unknown_chars(true);

    spawner.spawn(display::render(spi_bus_epd, epd_busy, epd_rst, epd_dc).unwrap());

    let mut display: display::EpdDisplay = display::EpdDisplay::default();
    

    #[cfg(not(feature = "epd2in7"))]
    display.set_rotation(DisplayRotation::Rotate90);
    

    let sdcard = SdCard::new_with_options(spi_bus_sd, embassy_time::Delay, AcquireOpts { use_crc: false, acquire_retries: 50 });
    let volume_mgr = VolumeManager::new(sdcard, crate::sd_mount::TimeSource);
    let sd_mount = SdMount::new(volume_mgr);
    crate::sd_mount::SD_MOUNT.lock().await.replace(sd_mount);
    // SD上电后要通信一次，不然对显示通信有干扰
    if let Some(ref mut sd) = *SD_MOUNT.lock().await {
        let _ = sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0));
    }

    let mut need_ap = false;
    loop {
        println!("entry need_ap 1");
        if let Some(wifi) = storage::WIFI_INFO.lock().await.as_ref() {
            println!("wifi_finish:{:?}", wifi.wifi_finish);
            println!("wifi_ssid:{:?}", wifi.wifi_ssid);
            if !wifi.wifi_finish {
                need_ap = true;
            }
            println!("entry need_ap 2");
            break;
        }
        println!("entry need_ap");
        Timer::after(Duration::from_millis(50)).await;
    }

    let rtc_io = static_cell::make_static!(rtc_pin);
    add_rtcio(rtc_io, WakeupLevel::Low).await;

    if need_ap {
        use crate::pages::Page;
        println!("entry ap");
        WIFI_MODEL.lock().await.replace(WifiModel::AP);
        println!("wifi_model:{:?}", WIFI_MODEL.lock().await.as_ref());
        let _stack = crate::wifi::start_wifi_ap(
            &spawner,
            Rng::new(),
            peripherals.WIFI,
        ).await;

        loop {
            let mut qrcode_page = pages::setting_page::SettingPage::new();
            qrcode_page.bind_event().await;
            qrcode_page.run(spawner.clone()).await;
            Timer::after(Duration::from_secs(50)).await;
        }
    } else {
        spawner.spawn(crate::battery::test_bat_adc().unwrap());
        refresh_active_time().await;
        spawner.spawn(crate::worldtime::ntp_worker().unwrap());
        Timer::after_millis(10).await;
        spawner.spawn(pages::main_task(spawner.clone()).unwrap());

        WIFI_MODEL.lock().await.replace(WifiModel::STA);
        let _stack = crate::wifi::connect_wifi(
            &spawner,
            Rng::new(),
            peripherals.WIFI,
        ).await;
    }

    loop {
        if let Some(clock) = worldtime::get_clock() {
            let local = clock.local().await;
            let hour = local.hour();
            let minute = local.minute();
            let second = local.second();
            let str = format_args!("{:02}:{:02}:{:02}", hour, minute, second).to_string();
            println!("Current_time: {} {}", clock.get_date_str().await, str);
        }

        to_sleep_tips(Duration::from_secs(0), Duration::from_secs(180), true).await;
        Timer::after(Duration::from_secs(5)).await;
    }
}


fn draw_text(display: &mut display::EpdDisplay, text: &str, x: i32, y: i32) {
    let style = MonoTextStyleBuilder::new()
        .font(&embedded_graphics::mono_font::ascii::FONT_6X10)
        .text_color(White)
        .background_color(Black)
        .build();

    let text_style = TextStyleBuilder::new().baseline(Baseline::Top).build();

    let _ = Text::with_text_style(text, Point::new(x, y), style, text_style).draw(display);
}
