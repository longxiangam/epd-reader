#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(generic_const_exprs)]
#![allow(unused_must_use)]

mod display;
mod epd2in9_txt;
mod event;
mod model;
mod pages;
mod random;
mod request;
mod sd_mount;
mod sleep;
mod utils;
mod weather;
mod widgets;
mod wifi;
mod worldtime;
//mod web_service;
mod aht20;
mod battery;
mod board;
mod flash_sleep;
mod i2c_bus;
mod location;
mod panic;
mod pcf8563;
mod storage;
mod ttf_sd;
#[cfg(feature = "ttf_spike")]
mod ttf_spike;
mod web_service;

extern crate alloc;
use core::cell::RefCell;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_graphics::Drawable;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::prelude::Point;
use embedded_graphics::text::{Baseline, Text, TextStyleBuilder};
use embedded_sdmmc::{SdCard, VolumeManager, sdcard::spi::AcquireOpts};

use esp_hal::{
    Config as HalConfig,
    analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation},
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull, RtcPin, RtcPinWithResistors},
    init as hal_init,
    interrupt::software::SoftwareInterruptControl,
    rng::Rng,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
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

use crate::battery::Battery;
use crate::sd_mount::{SD_MOUNT, SdMount};
use crate::sleep::{add_rtcio, refresh_active_time, to_sleep_tips};
use crate::wifi::{WIFI_MODEL, WifiModel};
use alloc::string::ToString;
use epd_waveshare::graphics::DisplayRotation;
use esp_hal::rtc_cntl::sleep::WakeupLevel;

use critical_section::Mutex as CsMutex;

esp_bootloader_esp_idf::esp_app_desc!();

/// 堆缓冲放进独立的 dram2 段（bootloader 释放的 ~64KB DRAM，原本空闲）。
/// 好处：主 DRAM 全部留给栈（TTF 渲染异步链深也能容纳，64KB 堆放主 DRAM 时曾栈溢出），
/// 堆又够用（WiFi ~60KB / TtfWs 44KB），且栈溢出看门狗（_stack_end 处 watchpoint）不受影响。
/// 阅读与正常模式共用这一块堆，无需按模式区分。
#[unsafe(link_section = ".dram2_uninit")]
static mut DRAM2_HEAP: core::mem::MaybeUninit<[u8; 65280]> = core::mem::MaybeUninit::uninit();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // 堆放进 dram2 独立段（不占主 DRAM）→ 主 DRAM 全给栈，堆栈不再争抢。
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(DRAM2_HEAP) as *mut u8;
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            heap_ptr,
            65280,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }

    // TTF spike：基准 run() 会阻塞启动数秒，暂不在 boot 调用；
    // 改为在主页直接渲染矢量字做端到端验证（见 main_page render）。

    println!("entry");

    let config = HalConfig::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = hal_init(config);

    // pad hold：v22 释放上次深睡布下的 hold（不释放的话 GPIO1 的驱低上轨
    // 写不进去 → 轨断电 → 显示/SD 全灭）；旧板给 GPIO1(SD 轨) 打 hold
    crate::board::init_pad_hold(&mut peripherals);

    // I2C 总线（v22：GPIO9=SDA GPIO21=SCL，挂 AHT20 + PCF8563；旧板无）——
    // 在任何外设被 move 之前构造
    let i2c = crate::board::init_i2c(&mut peripherals);
    // MISO(GPIO10) 内部上拉（v22：SD 兼容；需在 SPI 占用 GPIO10 前配置）
    crate::board::sd_miso_pullup(&mut peripherals);
    // 电源轨须在 EPD 等引脚 move 出 peripherals 之前初始化
    crate::board::init_power_rails(&mut peripherals).await;

    // Timer for embassy runtime
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // v22 诊断：rtc_fast 状态自增——跨深睡唤醒保持递增 = 保留正常。
    // 先等 3 秒：唤醒重启后 USB 重枚举需要时间，不等则最开始的打印会丢。
    //esp_hal::delay::Delay::new().delay_millis(3000);
    //crate::board::print_boot_diag();

    // RTC
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    crate::sleep::RTC_MANGE.lock().await.replace(rtc);

    // Init storage
    crate::storage::enter_process().await;
    // 加载全刷间隔缓存（render 任务会读取）
    crate::display::reload_full_refresh_times();

    trace!("test trace");

    let reason =
        esp_hal::system::reset_reason().unwrap_or(esp_hal::rtc_cntl::SocResetReason::ChipPowerOn);
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
    let epd_miso = unsafe { peripherals.GPIO10.clone_unchecked() };

    let sdcard_cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());

    // SPI bus
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(32))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(epd_sclk)
    .with_miso(epd_miso)
    .with_mosi(epd_mosi);

    // 按键梯 + 数字直连键按板型取自 board（v22 仅梯；旧板另有 GPIO9 键1）
    if let Some(i2c) = i2c {
        crate::i2c_bus::I2C.lock().await.replace(i2c);
    }

    // 深睡唤醒分类：PCF8563 TF=1 → PCF 定时唤醒（PCF INT 拉低 GPIO4）；
    // ESP RTC Timer → PCF 布防失败时的 ESP 定时回退唤醒；否则按钮唤醒。
    // 读完清标志 + 关定时器（INT 释放，防运行期假按键）
    let timed_wake = crate::pcf8563::consume_timer_wake().await
        || matches!(wake_reason, esp_hal::system::SleepSource::Timer);
    // 天气数据过期才需要联网刷新（8 秒窗口等 WiFi+同步）；
    // 数据新鲜则无窗口：读完温度重绘后立刻回睡
    let stale = crate::weather::Weather::weather_stale().await;
    crate::sleep::set_timed_wake(timed_wake, if stale { 8_000 } else { 0 });
    // 非定时唤醒（按钮/上电）进入程序：首帧强制全刷，清上一会话残影；
    // 定时唤醒保持快刷（分钟级唤醒每次全刷会闪烁+耗电）。
    if !timed_wake {
        crate::display::mark_need_clear();
    }
    println!(
        "[wake] pcf timer={} timed_wake={} stale={} reason={:?}",
        timed_wake, timed_wake, stale, wake_reason
    );

    // ADC setup（按键梯 / 电池分压——引脚按板型见 board）
    let mut adc1_config = AdcConfig::new();
    let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
        crate::board::key_ladder_pin(),
        Attenuation::_11dB,
    );
    let bat_adc1_pin = adc1_config
        .enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
            crate::board::bat_pin(),
            Attenuation::_11dB,
        );

    let adc1 = Adc::new(peripherals.ADC1, adc1_config);
    event::ADC_PER.lock().await.replace(adc1);
    event::ADC_PIN.lock().await.replace(adc1_pin);

    // 按键脚数字输入 + 上拉——在 ADC 初始化之后再配一次（set_analog 会剥掉）
    {
        let k2_in = Input::new(
            crate::board::key_ladder_pin(),
            InputConfig::default().with_pull(Pull::Up),
        );
        core::mem::forget(k2_in);
    }

    // 板载传感器任务（AHT20 温湿度）
    crate::board::spawn_sensors(&spawner);

    // 按键任务：v22 单梯（GPIO4）；旧板 数字键 GPIO9 + 梯 GPIO2
    #[cfg(feature = "board_v2")]
    spawner.spawn(event::run(crate::board::key_ladder_pin()).unwrap());
    #[cfg(feature = "board_v1")]
    spawner.spawn(
        event::run(
            crate::board::key_direct_pin(),
            crate::board::key_ladder_pin(),
        )
        .unwrap(),
    );
    // 事件派发任务：从事件队列串行派发到监听回调（扫描与派发解耦，长回调不丢键）
    spawner.spawn(event::dispatch().unwrap());

    let battery = Battery::new();
    battery::BATTERY.lock().await.replace(battery);
    battery::ADC_PIN.lock().await.replace(bat_adc1_pin);

    // GPIO2（电池分压脚）深睡隔离：slp_sel=1 + sleep 路径 IE/WPU/WPD 全 0。
    // GPIO2 是 strapping 脚，硬件复位默认带内部上拉——深睡数字域掉电后 pad 若
    // 回退到复位默认，会把分压中点（VBAT/2≈1.7V）抬到 ~3V，徒增漏电。显式
    // sleep 配置在 RTC 域，深睡中强制高阻。运行期 pad 仍走 FUN_* 模拟配置，
    // ADC 采样不受影响。set_analog 与 isolate_digital_gpio 都不碰这些位，
    // 开机设一次终身有效。
    {
        let io_mux = esp_hal::peripherals::IO_MUX::regs();
        io_mux.gpio(2).modify(|_, w| {
            w.slp_sel().set_bit();
            w.mcu_ie().clear_bit();
            w.mcu_wpu().clear_bit();
            w.mcu_wpd().clear_bit()
        });
    }

    // Shared SPI bus using critical section
    let shared_spi = CsMutex::new(RefCell::new(spi));
    let shared_spi_static = static_cell::make_static!(shared_spi);

    let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
    let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();

    let spi_bus_sd = static_cell::make_static!(spi_bus_sd);
    let spi_bus_epd = static_cell::make_static!(spi_bus_epd);

    let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>();
    let _font = font.with_ignore_unknown_chars(true);

    // 显示控制脚驱动化（v22 dc=GPIO21，构造处按 feature 区分具体 GPIO）
    let epd_busy_in = Input::new(
        epd_busy,
        esp_hal::gpio::InputConfig::default().with_pull(Pull::Down),
    );
    let epd_rst_out = Output::new(epd_rst, Level::High, OutputConfig::default());
    let epd_dc_out = Output::new(epd_dc, Level::High, OutputConfig::default());
    spawner.spawn(display::render(spi_bus_epd, epd_busy_in, epd_rst_out, epd_dc_out).unwrap());

    let mut display: display::EpdDisplay = display::EpdDisplay::default();

    #[cfg(not(feature = "epd2in7"))]
    display.set_rotation(DisplayRotation::Rotate90);

    let sdcard = SdCard::new_with_options(
        spi_bus_sd,
        embassy_time::Delay,
        AcquireOpts {
            use_crc: false,
            acquire_retries: 50,
        },
    );
    let volume_mgr = VolumeManager::new(sdcard, crate::sd_mount::TimeSource);
    let sd_mount = SdMount::new(volume_mgr);
    crate::sd_mount::SD_MOUNT.lock().await.replace(sd_mount);
    // SD上电后要通信一次，不然对显示通信有干扰
    if let Some(ref mut sd) = *SD_MOUNT.lock().await {
        match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
            Ok(_) => println!("[sd] volume open ok"),
            Err(e) => println!("[sd] open_volume fail: {:?}", e),
        }
    }

    // RTC 唤醒键注册：必须在阅读分支之前——阅读模式睡眠(to_sleep_tips/reboot_sleep)
    // 也要靠这个 RTC IO 唤醒源，否则睡下唤不醒。
    let rtc_io = static_cell::make_static!(crate::board::key_ladder_pin());
    add_rtcio(rtc_io, WakeupLevel::Low).await;

    // 重启分模式：阅读模式启动时不初始化 WiFi，独占整块堆做 TTF（大字形缓存）。
    // 模式标志存 rtc_fast（跨 reboot_sleep 深睡保留）；直接进 reading_task。
    // v22：IO（RTCIO）唤醒会丢 rtc_fast 的 READING_MODE——从 NVS 恢复"上次在阅读"，
    // 直接走阅读模式（不连网），与旧行为一致。
    if crate::storage::read_sleep_page() == Some(crate::storage::SleepPageStorage::PAGE_READING) {
        unsafe {
            core::ptr::addr_of_mut!(crate::pages::read::READING_MODE).write(true);
        }
        println!("[boot] NVS: 上次在阅读 → 恢复阅读模式");
    }
    {
        let reading_mode = unsafe { *core::ptr::addr_of!(crate::pages::read::READING_MODE) };
        if reading_mode {
            println!("[boot] 阅读模式：跳过 WiFi，独占堆做 TTF");
            // v22：阅读模式没有 ntp_worker，用 PCF8563 恢复时钟
            //（时间显示/跨天判断不再因无 WiFi 而滞后）
            crate::worldtime::init_clock_pcf().await;
            refresh_active_time().await;
            // 阅读模式也采样电池（60s 一轮，状态行显示用）
            spawner.spawn(crate::battery::test_bat_adc().unwrap());
            spawner.spawn(crate::pages::reading_task(spawner.clone()).unwrap());
            // main 不返回即可；睡眠/交互由 reading_task(ReadPage) 处理
            loop {
                Timer::after(Duration::from_secs(5)).await;
            }
        }
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

    if need_ap {
        use crate::pages::Page;
        println!("entry ap");
        WIFI_MODEL.lock().await.replace(WifiModel::AP);
        println!("wifi_model:{:?}", WIFI_MODEL.lock().await.as_ref());
        let _stack = crate::wifi::start_wifi_ap(&spawner, Rng::new(), peripherals.WIFI).await;

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

        // 正常模式开机连 WiFi。阅读走独立重启模式（READING_MODE），不在此连。
        {
            WIFI_MODEL.lock().await.replace(WifiModel::STA);
            let _stack = crate::wifi::connect_wifi(&spawner, Rng::new(), peripherals.WIFI).await;
        }
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

        to_sleep_tips(
            Duration::from_secs(0),
            crate::sleep::idle_for(Duration::from_secs(180)),
            true,
        )
        .await;
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
