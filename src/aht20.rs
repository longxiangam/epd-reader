//! v22：AHT20 温湿度传感器（0x38）驱动 + 周期采样任务。
//!
//! 协议：上电等 40ms → 状态字(直接读) bit3=1 表示已校准，否则发 0xBE 初始化；
//! 测量发 [0xAC,0x33,0x00] → 等 80ms → 读 7 字节（状态+湿度20bit+温度20bit+CRC8）。
//! 数据存原子量，渲染侧同步读取快照（无锁）。

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use embassy_time::Timer;
use esp_println::println;

use crate::i2c_bus;

pub const ADDR: u8 = 0x38;

/// 室内温度（℃）×10；i32::MIN = 无数据
static INDOOR_T: AtomicI32 = AtomicI32::new(i32::MIN);
/// 室内湿度（%）
static INDOOR_H: AtomicU32 = AtomicU32::new(0);
/// 传感器就绪（init 成功过一次）
static READY: AtomicBool = AtomicBool::new(false);

/// 渲染侧快照：Some((温度℃×10, 湿度%))
pub fn indoor_snapshot() -> Option<(i32, u32)> {
    let t = INDOOR_T.load(Ordering::Relaxed);
    if t == i32::MIN {
        None
    } else {
        Some((t, INDOOR_H.load(Ordering::Relaxed)))
    }
}

/// 首次采样是否已完成（唤醒后页面渲染前等待用）
pub fn has_data() -> bool {
    INDOOR_T.load(Ordering::Relaxed) != i32::MIN
}

async fn read_status() -> Option<u8> {
    let mut b = [0u8; 1];
    i2c_bus::write_read(ADDR, &[], &mut b).await.ok()?;
    Some(b[0])
}

/// 上电初始化：等稳定 + 必要时发校准命令。返回是否就绪。
pub async fn init() -> bool {
    Timer::after_millis(100).await; // 上电稳定（datasheet 要求 ≥40ms，留余量）
    let Some(status) = read_status().await else {
        println!("[aht20] not found");
        return false;
    };
    if status & 0x08 == 0 {
        // 未校准：发初始化命令 0xBE
        if i2c_bus::write(ADDR, &[0xBE, 0x08, 0x00]).await.is_err() {
            println!("[aht20] init cmd fail");
            return false;
        }
        Timer::after_millis(10).await;
    }
    READY.store(true, Ordering::Relaxed);
    true
}

/// CRC8（poly 0x31, init 0xFF），校验 7 字节帧的前 6 字节
fn crc_ok(b: &[u8; 7]) -> bool {
    let mut crc: u8 = 0xFF;
    for &byte in b[..6].iter() {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x31;
            } else {
                crc <<= 1;
            }
        }
    }
    crc == b[6]
}

/// 触发一次测量 → (温度℃×10, 湿度%)。整数运算，无浮点。
pub async fn measure() -> Option<(i32, u32)> {
    if !READY.load(Ordering::Relaxed) {
        return None;
    }
    if i2c_bus::write(ADDR, &[0xAC, 0x33, 0x00]).await.is_err() {
        println!("[aht20] trigger fail");
        return None;
    }
    Timer::after_millis(80).await; // 测量完成时间（datasheet ≤80ms）
    let mut b = [0u8; 7];
    if i2c_bus::write_read(ADDR, &[], &mut b).await.is_err() {
        println!("[aht20] read fail");
        return None;
    }
    if b[0] & 0x80 != 0 {
        println!("[aht20] busy");
        return None; // 仍忙
    }
    if !crc_ok(&b) {
        println!("[aht20] crc mismatch");
        return None;
    }
    let humi_raw = ((b[1] as u32) << 12) | ((b[2] as u32) << 4) | (b[3] >> 4) as u32;
    let humi = humi_raw * 100 / (1 << 20);
    let temp_raw = (((b[3] & 0x0F) as u32) << 16) | ((b[4] as u32) << 8) | b[5] as u32;
    // temp = raw / 2^20 * 200 - 50（℃），×10 保留一位小数
    let temp = (temp_raw as i64 * 2000 / (1 << 20)) as i32 - 500;
    Some((temp, humi))
}

/// 周期采样：唤醒期间每 60s 更新一次室内温湿度
#[embassy_executor::task]
pub async fn sample_task() -> ! {
    if !init().await {
        // 找不到传感器就低频重试（热插拔/上电慢兜底）
        loop {
            Timer::after_secs(30).await;
            if init().await {
                break;
            }
        }
    }
    println!("[aht20] ready");

    let mut fail_round = 0u32;
    loop {
        // 唤醒后首次测量常因传感器忙而失败（深睡打断测量周期）：
        // 失败快速重试 3 次（间隔 200ms），避免空窗 60 秒
        let mut got: Option<(i32, u32)> = None;
        for _ in 0..3 {
            match measure().await {
                Some((t, h)) => {
                    got = Some((t, h));
                    break;
                }
                None => Timer::after_millis(200).await,
            }
        }

        match got {
            Some((t, h)) => {
                fail_round = 0;
                INDOOR_T.store(t, Ordering::Relaxed);
                INDOOR_H.store(h, Ordering::Relaxed);
                println!("[aht20] sample t={}.{}℃ h={}%", t / 10, t % 10, h);
            }
            None => {
                fail_round += 1;
                println!("[aht20] measure failed x{}", fail_round);
                // 连续失败清空显示（避免长期陈旧值），下轮重试
                INDOOR_T.store(i32::MIN, Ordering::Relaxed);
            }
        }
        Timer::after_secs(60).await;
    }
}
