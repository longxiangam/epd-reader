//! v22：三按键全部走 ADC 分压检测（GPIO2 单脚电阻梯）。
//!
//! 硬件：三个按键经不同分压电阻接到 GPIO2（ADC1），未按下被上拉拉高。
//! 软件三层：
//! 1. 校准（开机无有效数据 / 运行期长按任意键 15s 逃逸）：逐键实测 ADC 值存 NVS；
//! 2. 识别：按下值与校准值最近邻匹配，带宽 = 相邻间隔的一半（夹到 [40,300]）；
//! 3. 15s 逃逸监控：基于**开机时实测的 idle 基线**（不依赖可能失准的 NVS 数据），
//!    采样值持续偏离基线 15s → 重新进入校准，保证失准后仍可操作。

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::prelude::{Dimensions, Point};
use epd_waveshare::prelude::Display as EpdDisplayTrait;
use esp_println::println;
use u8g2_fonts::FontRenderer;
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use crate::event::{self, EventType};
use crate::sleep::refresh_active_time;
use crate::storage::{AdcCalStorage, NvsStorage};

/// 与开机 idle 基线的偏离超过该值视为"有键按下"（12bit ADC，分压键值偏离数百以上）
const DEVIATION_MARGIN: i32 = 300;
/// 任意键持续按住该时长 → 重新进入校准（逃逸通道）
const ESCAPE_HOLD_MS: u64 = 15_000;

/// 校准界面进行中标志：监控任务据此暂停逃逸判定（校准时按住按键是正常操作）
static IN_CALIBRATION: AtomicBool = AtomicBool::new(false);
/// 睡眠入口置位：让 key_task 停止 ADC 轮询（深睡掉电时 SAR ADC 不能有挂起状态）
static ADC_PAUSED: AtomicBool = AtomicBool::new(false);

// ── ADC 采样 ──

/// ADC 单脚多次采样取均值。偶发 Err 重试（冷启动常见），总尝试超限返回 None。
///
/// 连续 30 次 WouldBlock 视为转换状态机卡死（esp-hal 硬件 errata：高 APB 时钟下
/// onetime_start 脉冲可能未被捕获，Adc 实例的 active_channel 永久停在启动态，
/// 同通道永远 WouldBlock、异通道直接拒绝）→ 重建 ADC 实例清状态。
pub async fn adc_avg(samples: usize) -> Option<u16> {
    let mut need = samples;
    let mut sum: u32 = 0;
    let mut tries = 0usize;
    let mut stuck = 0u32;
    while need > 0 {
        tries += 1;
        if tries > 200 {
            println!("[adc_key] adc too many tries");
            heal_adc().await;
            return None;
        }
        let mut got = false;
        {
            let mut pin_guard = event::ADC_PIN.lock().await;
            let mut adc_guard = event::ADC_PER.lock().await;
            if let (Some(pin), Some(adc)) = (pin_guard.as_mut(), adc_guard.as_mut()) {
                match adc.read_oneshot(pin) {
                    Ok(v) => {
                        sum = sum.saturating_add(v as u32);
                        need -= 1;
                        got = true;
                    }
                    Err(_) => {}
                }
            }
        } // 锁在此释放，重试等待期间不阻塞 battery/按键互读
        if got {
            stuck = 0;
        } else {
            stuck += 1;
            if stuck >= 30 {
                heal_adc().await;
                return None;
            }
            Timer::after_millis(2).await;
        }
    }
    Some((sum / samples as u32) as u16)
}

/// 重建 ADC 实例（steal ADC1 + 重配按键 GPIO2 / 电池 GPIO4 双通道），
/// 清掉 read_oneshot 卡死的 active_channel 状态。调用前必须已释放 ADC 相关锁。
async fn heal_adc() {
    use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
    println!("[adc_key] ADC 转换卡死，重建实例");
    unsafe {
        let mut cfg = AdcConfig::new();
        let key_pin = cfg.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
            esp_hal::peripherals::GPIO2::steal(),
            Attenuation::_11dB,
        );
        let bat_pin = cfg.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
            esp_hal::peripherals::GPIO4::steal(),
            Attenuation::_11dB,
        );
        let adc = Adc::new(esp_hal::peripherals::ADC1::steal(), cfg);
        crate::event::ADC_PER.lock().await.replace(adc);
        crate::event::ADC_PIN.lock().await.replace(key_pin);
        crate::battery::ADC_PIN.lock().await.replace(bat_pin);
    }
}

/// 一轮快速去抖：20 次采样中偏离 idle 基线的次数
async fn pressed_burst(idle: u16) -> u32 {
    let mut count = 0;
    for _ in 0..20 {
        if let Some(v) = adc_avg(1).await {
            if (v as i32 - idle as i32).abs() > DEVIATION_MARGIN {
                count += 1;
            }
        }
    }
    count
}

/// 中位数采样：抗机械弹跳/过渡期的瞬态毛刺（均值会被单次离群值带偏，
/// 中位数要求一半以上样本离群才会失真）。
async fn adc_median(samples: usize) -> Option<u16> {
    const MAXN: usize = 21;
    let n = samples.min(MAXN);
    let mut buf = [0u16; MAXN];
    let mut filled = 0usize;
    while filled < n {
        match adc_avg(1).await {
            Some(v) => {
                buf[filled] = v;
                filled += 1;
            }
            None => return None,
        }
    }
    // 插入排序（n ≤ 21）
    for i in 1..n {
        let mut j = i;
        let key = buf[j];
        while j > 0 && buf[j - 1] > key {
            buf[j] = buf[j - 1];
            j -= 1;
        }
        buf[j] = key;
    }
    Some(buf[n / 2])
}

// ── 识别 ──

/// 按下值 → 键号（0=无/抖动）。最近邻匹配，带宽 = 与相邻值（含 idle）最小间隔的一半。
fn classify(avg: u16, cal: &AdcCalStorage) -> u32 {
    let a = avg as i32;
    let refs: [(u32, i32); 3] = [
        (1, cal.key1 as i32),
        (2, cal.key2 as i32),
        (3, cal.key3 as i32),
    ];
    // 最近邻
    let mut best_num = refs[0].0;
    let mut best_val = refs[0].1;
    let mut best_dist = (a - refs[0].1).abs();
    for &(num, kv) in refs.iter() {
        let d = (a - kv).abs();
        if d < best_dist {
            best_num = num;
            best_val = kv;
            best_dist = d;
        }
    }
    // 带宽 = 该键与其它键值及 idle 的最小间隔的一半，夹到 [40, 300]
    let mut min_gap = i32::MAX;
    for &(_, ov) in refs.iter() {
        if ov == best_val {
            continue;
        }
        let g = (best_val - ov).abs();
        if g < min_gap {
            min_gap = g;
        }
    }
    let gi = (best_val - cal.idle as i32).abs();
    if gi < min_gap {
        min_gap = gi;
    }
    let band = (min_gap / 2).clamp(40, 300);
    if best_dist <= band {
        best_num
    } else {
        0 // 介于两键之间/抖动
    }
}

/// 读 NVS 校准数据（无效返回 None）
fn load_cal() -> Option<AdcCalStorage> {
    AdcCalStorage::read().ok().filter(|c| c.valid())
}

// ── 按键任务（v22 模式下替代 GPIO 边沿检测）──

/// 开机采样一次的 idle 基线：此刻用户不应按键。逃逸监控只认这个基线，
/// 与 NVS 校准数据是否失准无关 —— 这是"失准后仍可逃逸"的关键。
struct EscapeMonitor {
    dev_since_ms: u64, // 0 = 未在偏离状态
}

impl EscapeMonitor {
    fn tick(&mut self, avg: u16, boot_idle: u16) -> bool {
        let now = Instant::now().as_millis();
        if (avg as i32 - boot_idle as i32).abs() > DEVIATION_MARGIN {
            if self.dev_since_ms == 0 {
                self.dev_since_ms = now;
            } else if now - self.dev_since_ms >= ESCAPE_HOLD_MS {
                self.dev_since_ms = 0;
                return true;
            }
        } else {
            self.dev_since_ms = 0;
        }
        false
    }
}

enum DetectOutcome {
    Done,
    Escalate,
}

/// 按键事件检测：镜像 event::key_detection 的 短按/双击/长按 状态机，
/// 区别仅在"是否按下"由 ADC 采样判定（识别已在外部完成）。
/// 按住 60ms 时做一次键号复判：纠正极少数初始判定仍落在过渡期的情况
/// （只纠一次，之后不再改号，避免按住中途换键事件）。
async fn key_detection_adc(mut num: u32, cal: AdcCalStorage, boot_idle: u16) -> DetectOutcome {
    const LONG_ING_INTERVAL_MS: u64 = 100;
    let begin_ms = Instant::now().as_millis();
    let mut last_long_ing_ms = begin_ms;
    let mut is_long = false;
    let mut rechecked = false;

    loop {
        let pressed_times = pressed_burst(boot_idle).await;

        if pressed_times > 16 {
            // 按下保持
            let current = Instant::now().as_millis();
            // 触点完全稳定后复判一次键号
            if !rechecked && current - begin_ms >= 60 {
                rechecked = true;
                if let Some(v) = adc_median(15).await {
                    let n2 = classify(v, &cal);
                    if n2 != 0 && n2 != num {
                        println!("[adc_key] key recheck {} -> {}", num, n2);
                        num = n2;
                    }
                }
            }
            // 逃逸通道：按住超 15s（不分键、不看校准数据）
            if current - begin_ms >= ESCAPE_HOLD_MS {
                return DetectOutcome::Escalate;
            }
            if current - begin_ms > 500 {
                if !is_long {
                    is_long = true;
                    last_long_ing_ms = current;
                    event::toggle_event(EventType::KeyLongStart(num), current).await;
                } else if current - last_long_ing_ms >= LONG_ING_INTERVAL_MS {
                    last_long_ing_ms = current;
                    event::toggle_event(EventType::KeyLongIng(num), current).await;
                }
            }
        } else if pressed_times < 2 {
            // 释放
            let current = Instant::now().as_millis();
            if is_long {
                event::toggle_event(EventType::KeyLongEnd(num), current).await;
                return DetectOutcome::Done;
            }
            // 短按：窗口内再次按下 = 双击
            loop {
                let current = Instant::now().as_millis();
                if !*event::ENABLE_DOUBLE.lock().await {
                    event::toggle_event(EventType::KeyShort(num), current).await;
                    return DetectOutcome::Done;
                }
                if current - begin_ms > 400 {
                    event::toggle_event(EventType::KeyShort(num), current).await;
                    return DetectOutcome::Done;
                }
                let again = pressed_burst(boot_idle).await;
                if again > 16 {
                    event::toggle_event(EventType::KeyDouble(num), current).await;
                    return DetectOutcome::Done;
                }
            }
        }
        Timer::after(Duration::from_millis(1)).await;
    }
}

/// 逃逸触发：清掉所有页面监听 → 校准 → 重启（reboot_sleep 深睡 50ms 唤醒重启，
/// 保留 rtc_fast 的模式/书签/续读状态）
async fn escalate(mon: &mut EscapeMonitor) -> ! {
    mon.dev_since_ms = 0;
    println!("[adc_key] 任意键长按15秒 → 重新校准");
    run_calibration(true).await;
    loop {
        Timer::after_secs(1).await; // run_calibration 内部会重启，不会走到这里
    }
}

/// v22 按键主任务：轮询 ADC → 逃逸监控 → 识别 → 事件检测。永不返回。
pub async fn key_task() -> ! {
    let boot_idle = loop {
        match adc_avg(20).await {
            Some(v) => break v,
            None => Timer::after_millis(10).await,
        }
    };
    println!("[adc_key] boot idle={}", boot_idle);
    let mut mon = EscapeMonitor { dev_since_ms: 0 };

    loop {
        // 睡眠入口暂停轮询：深睡掉电时 SAR ADC 不能有挂起的转换状态
        if ADC_PAUSED.load(Ordering::Relaxed) {
            Timer::after_millis(50).await;
            continue;
        }

        let avg = match adc_avg(8).await {
            Some(v) => v,
            None => {
                Timer::after_millis(20).await;
                continue;
            }
        };

        // 15s 逃逸监控：校准进行中不判定（校准本来就要按住按键）
        if IN_CALIBRATION.load(Ordering::Relaxed) {
            mon.dev_since_ms = 0;
        } else if mon.tick(avg, boot_idle) {
            escalate(&mut mon).await;
        }

        // 校准中不产生按键事件
        if IN_CALIBRATION.load(Ordering::Relaxed) {
            Timer::after_millis(50).await;
            continue;
        }

        // 识别 + 事件检测
        if let Some(cal) = load_cal() {
            let rough = classify(avg, &cal);
            if rough != 0 {
                // 关键：等触点稳定再定键号。按下瞬间的机械弹跳+分压过渡期采样
                // 会落在两键电压带之间，最近邻匹配就会串键 —— 这是互串的根源。
                // 30ms 稳定等待 + 中位数采样后再分类；落不进任何键带则视为毛刺丢弃。
                Timer::after_millis(30).await;
                let num = adc_median(15).await.map(|v| classify(v, &cal)).unwrap_or(0);
                if num != 0 {
                    refresh_active_time().await;
                    match key_detection_adc(num, cal, boot_idle).await {
                        DetectOutcome::Done => {}
                        DetectOutcome::Escalate => escalate(&mut mon).await,
                    }
                    refresh_active_time().await;
                }
            }
        }
        Timer::after_millis(15).await;
    }
}

// ── 校准 ──

/// 电子纸显示一行/两行提示并刷新（居中，跟随各屏 feature 的旋转映射）
async fn show_screen(line1: &str, line2: Option<&str>) {
    // 等 display 渲染任务就绪（DISPLAY 静态由渲染任务写入）
    let mut waited = 0u32;
    while crate::display::display_mut().is_none() {
        Timer::after_millis(20).await;
        waited += 1;
        if waited > 250 {
            println!("[adc_key] display not ready");
            return;
        }
    }
    if let Some(display) = crate::display::display_mut() {
        use epd_waveshare::color::{Black, Color};
        display.clear_buffer(Color::White);
        let font: FontRenderer = FontRenderer::new::<fonts::u8g2_font_wqy14_t_gb2312>()
            .with_ignore_unknown_chars(true);
        // Rotate90 下视觉坐标与 buffer 坐标互换（同 display::show_error 的处理）
        let c = display.bounding_box().center();
        let _ = font.render_aligned(
            line1,
            Point::new(c.y, c.x - 14),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(Black),
            display,
        );
        if let Some(sub) = line2 {
            let _ = font.render_aligned(
                sub,
                Point::new(c.y, c.x + 14),
                VerticalPosition::Center,
                HorizontalAlignment::Center,
                FontColor::Transparent(Black),
                display,
            );
        }
    }
    crate::display::RENDER_CHANNEL
        .send(crate::display::RenderInfo { time: 0, need_sleep: false })
        .await;
    crate::display::await_render_idle().await;
}

/// 采样稳定 idle：连续两组均值接近才算稳（用户可能在按 → 重采）
async fn capture_idle() -> u16 {
    loop {
        let a = adc_avg(40).await.unwrap_or(0);
        Timer::after_millis(50).await;
        let b = adc_avg(20).await.unwrap_or(0);
        if a > 0 && (a as i32 - b as i32).abs() < 80 {
            return a;
        }
    }
}

async fn wait_pressed(idle: u16) {
    loop {
        let v = adc_avg(8).await.unwrap_or(idle);
        if (v as i32 - idle as i32).abs() > DEVIATION_MARGIN {
            return;
        }
        Timer::after_millis(10).await;
    }
}

async fn wait_released(idle: u16) {
    loop {
        let v = adc_avg(8).await.unwrap_or(idle);
        if (v as i32 - idle as i32).abs() < 150 {
            return;
        }
        Timer::after_millis(10).await;
    }
}

/// 校准流程：idle → 依次按住/松开按键1/2/3 → 间隔校验 → 存 NVS。
/// escalated=false（开机校准）：完成后返回继续正常启动；
/// escalated=true（15s 逃逸）：完成后 reboot_sleep 重启回到正常模式。
pub async fn run_calibration(escalated: bool) {
    IN_CALIBRATION.store(true, Ordering::Relaxed);
    // 清掉页面监听：校准期间页面不应响应按键
    event::clear().await;

    loop {
        refresh_active_time().await;
        show_screen("按键校准", Some("请勿按任何按键")).await;
        let idle = capture_idle().await;
        println!("[adc_key] cal idle={}", idle);

        let mut keys = [0u16; 3];
        let mut ok = true;
        for n in 1..=3u32 {
            refresh_active_time().await;
            show_screen("请按住按键", Some(&fmt_step(n, false))).await;
            wait_pressed(idle).await;
            Timer::after_millis(150).await; // 等手按稳
            let v = adc_avg(40).await.unwrap_or(0);
            println!("[adc_key] key{} = {}", n, v);
            show_screen("请松开按键", Some(&fmt_step(n, true))).await;
            wait_released(idle).await;
            if v == 0 {
                ok = false;
                break;
            }
            keys[(n - 1) as usize] = v;
        }

        if !ok {
            show_screen("采样失败", Some("将重新开始")).await;
            continue;
        }

        let cal = AdcCalStorage {
            magic: AdcCalStorage::MAGIC,
            idle,
            key1: keys[0],
            key2: keys[1],
            key3: keys[2],
        };
        if !cal.valid() {
            show_screen("按键区分度不足", Some("请检查分压电阻后重试")).await;
            Timer::after_millis(800).await;
            continue;
        }
        match cal.write() {
            Ok(_) => {
                println!("[adc_key] cal saved: idle={} k1={} k2={} k3={}", idle, keys[0], keys[1], keys[2]);
                show_screen("校准完成", None).await;
            }
            Err(e) => {
                println!("[adc_key] cal save err: {:?}", e);
                show_screen("保存失败", Some("将重新校准")).await;
                Timer::after_millis(800).await;
                continue;
            }
        }
        break;
    }

    IN_CALIBRATION.store(false, Ordering::Relaxed);

    if escalated {
        // 重启回到正常模式（保留 rtc_fast）
        crate::sleep::reboot_sleep().await;
    }
}

/// 步骤副标题："松开后按 键1/3" 之类；captured=true 表示该键已采到
fn fmt_step(n: u32, _captured: bool) -> heapless::String<24> {
    let mut s: heapless::String<24> = heapless::String::new();
    let _ = core::fmt::write(&mut s, format_args!("按键 {}/3", n));
    s
}

/// 开机入口：无有效校准数据 → 进入校准界面（完成后返回继续正常启动）
pub async fn boot_calibration_if_needed() {
    if load_cal().is_none() {
        println!("[adc_key] 无有效校准数据，进入开机校准");
        run_calibration(false).await;
    }
}

/// 睡眠入口调用：暂停 ADC 轮询并重建 ADC 实例，确保深睡掉电时
/// SAR ADC 没有任何挂起的转换状态（挂起状态与低电平唤醒互相作用曾导致秒醒）。
pub async fn pause_for_sleep() {
    ADC_PAUSED.store(true, Ordering::Relaxed);
    // 等 key_task 完成当前一轮采样
    Timer::after_millis(60).await;
    heal_adc().await;
}

// ── regi2c SAR ADC 模拟配置快照/恢复 ──
//
// esp-hal 的 ADC 每次采样前都会通过 regi2c（模拟 I2C，RTC 域）写 ADC_SAR1 的
// 校准/参考配置（DREF、INITIAL_CODE、ENCAL_GND/REF）。该配置跨深睡、跨烧录保留：
// 残留会把 GPIO2 的深睡唤醒比较器拉低 → 睡眠秒醒（还会"毒化"随后烧录的其他程序，
// 拔电才恢复）。启动时（ADC 初始化前）快照上电默认值，每次深睡前恢复。

const SAR_I2C_BLOCK: u8 = 0x69; // REGI2C_SAR_I2C
const SAR_I2C_HOST: u8 = 0;
/// reg 0/1: ADC_SAR1_INITIAL_CODE, reg 2: ADC_SAR1_DREF, reg 7: ADC_SAR1_ENCAL_*
const SAR_REGS: [u8; 4] = [0, 1, 2, 7];

static mut REGI2C_SNAPSHOT: [u8; 4] = [0; 4];

unsafe extern "C" {
    fn esp_rom_regi2c_read(block: u8, block_hostid: u8, reg_add: u8) -> u8;
    fn rom_i2c_writeReg(block: u8, block_hostid: u8, reg_add: u8, indata: u8);
}

/// 开机、ADC 初始化之前调用：记录 SAR ADC 模拟配置的上电默认值
pub fn regi2c_capture() {
    unsafe {
        let mut snap = [0u8; 4];
        for (i, &reg) in SAR_REGS.iter().enumerate() {
            snap[i] = esp_rom_regi2c_read(SAR_I2C_BLOCK, SAR_I2C_HOST, reg);
        }
        core::ptr::addr_of_mut!(REGI2C_SNAPSHOT).write(snap);
        println!("[adc_key] regi2c snapshot {:02x?}", snap);
    }
}

/// 深睡前调用：把 SAR ADC 模拟配置恢复为上电默认值
pub fn regi2c_restore() {
    unsafe {
        let snap = core::ptr::addr_of!(REGI2C_SNAPSHOT).read();
        for (i, &reg) in SAR_REGS.iter().enumerate() {
            rom_i2c_writeReg(SAR_I2C_BLOCK, SAR_I2C_HOST, reg, snap[i]);
        }
        println!("[adc_key] regi2c restored {:02x?}", snap);
    }
}
