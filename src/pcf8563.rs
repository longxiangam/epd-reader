//! v22：PCF8563 硬件 RTC（0x51）。
//!
//! 角色：系统时钟源。NTP 对时成功后把 UTC 时间写入 PCF8563（CR2032 备电池维持）；
//! 每次唤醒/重启优先从它恢复 —— 深睡漂移、阅读模式无 WiFi、断电换电池等
//! 场景都不再丢时间（旧链路 rtc_fast 估算在阅读模式重启后会滞后，股票跨天
//! 判断曾因此出错）。
//!
//! 寄存器：秒(0x02, bit7=VL 无效标志) 分(0x03) 时(0x04) 日(0x05) 星期(0x06)
//! 月(0x07, bit7=世纪) 年(0x08)，全部 BCD。时间以 UTC 存取（Clock::local 负责 +8h）。

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::i2c_bus;

pub const ADDR: u8 = 0x51;
const REG_SECONDS: u8 = 0x02;

fn bcd2(v: u8) -> u8 {
    (v & 0x0F) + (v >> 4) * 10
}

fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// 读当前时间 → Unix 秒（UTC）。VL 置位（备用电池掉过电/首次上电）返回 None。
pub async fn read_time() -> Option<i64> {
    let mut b = [0u8; 7];
    i2c_bus::write_read(ADDR, &[REG_SECONDS], &mut b).await.ok()?;
    if b[0] & 0x80 != 0 {
        // VL=1：内部时间不可信
        return None;
    }
    let sec = bcd2(b[0] & 0x7F);
    let min = bcd2(b[1] & 0x7F);
    let hour = bcd2(b[2] & 0x3F);
    let day = bcd2(b[3] & 0x3F);
    let month = bcd2(b[5] & 0x1F);
    let year = 2000u32 + bcd2(b[6]) as u32;
    if !(1..=12).contains(&month) || day == 0 || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let date = Date::from_calendar_date(year as i32, Month::try_from(month as u8).ok()?, day)
        .ok()?;
    let t = Time::from_hms(hour, min, sec).ok()?;
    Some(PrimitiveDateTime::new(date, t).assume_utc().unix_timestamp())
}

/// 写当前时间（Unix 秒，UTC）。写入前置 STOP 防回滚撕裂，写完清 STOP。
pub async fn write_time(ts: i64) -> bool {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(ts) else {
        return false;
    };
    let sec = to_bcd(dt.second());
    let min = to_bcd(dt.minute());
    let hour = to_bcd(dt.hour());
    let day = to_bcd(dt.day());
    let weekday = dt.weekday().number_days_from_monday(); // 0..6，仅计数用
    let month = to_bcd(dt.month() as u8) & 0x1F;
    let year = to_bcd((dt.year() - 2000) as u8);

    // STOP=1 暂停走时
    if i2c_bus::write(ADDR, &[0x00, 0x20]).await.is_err() {
        return false;
    }
    let ok = i2c_bus::write(
        ADDR,
        &[REG_SECONDS, sec, min, hour, day, weekday, month, year],
    )
    .await
    .is_ok();
    // STOP=0 恢复走时
    let _ = i2c_bus::write(ADDR, &[0x00, 0x00]).await;
    ok
}

/// 时间有效性区间（2020-2100），与世界time 模块的 sanity 检查一致
pub fn ts_valid(ts: i64) -> bool {
    (1577836800..4102444800).contains(&ts)
}

// ── 倒计时定时器（INT 脚 → GPIO4，深睡定时唤醒）──
//
// 寄存器：0x0E Timer_control（bit7 TE 使能，bits1:0 TD 频率：00=4096Hz 01=64Hz
// 10=1Hz 11=1/60Hz）；0x0F Timer_A（倒计数值）。到 0 时 TF（Control_status_2
// bit2）置位、INT 脚拉低（TIE=1 时）。TF 置位后保持，需软件写 0 清除。

const REG_CTRL2: u8 = 0x01; // bit1 AIE / bit2 TF / bit3 AF / bit0 TIE
const REG_TIMER_CTRL: u8 = 0x0E; // bit7 TE，bits1:0 TD
const REG_ALARM_MIN: u8 = 0x09; // 报警：分（bit7=1 禁用比较；BCD，写 0x09 起自动连写 0x0A~0x0C）
const REG_ALARM_WD: u8 = 0x0C; // 报警：星期（禁用比较时写 0x80）
const REG_TIMER_A: u8 = 0x0F;

/// 深睡前布防倒计时定时器：count 分钟后 INT 拉低（1/60Hz 分频，最大 255 分钟 ≈ 4.25 小时）。
/// 适合天气刷新等长周期定时唤醒。
pub async fn arm_timer_minutes(count: u8) -> bool {
    // 停定时器 → 装载倒计时 → 清 TF/AF + 使能 TIE → 启动（TE=1，TD=1/60Hz）
    if i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x00]).await.is_err() {
        return false;
    }
    if i2c_bus::write(ADDR, &[REG_TIMER_A, count]).await.is_err() {
        return false;
    }
    if i2c_bus::write(ADDR, &[REG_CTRL2, 0x01]).await.is_err() {
        return false;
    }
    i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x83]).await.is_ok()
}

/// 深睡前布防倒计时定时器：count 秒后 INT 拉低（经 GPIO4 唤醒 CPU）。
/// count: 1..=255（1Hz 倒计时，最长 4 分 15 秒）。
pub async fn arm_timer_secs(count: u8) -> bool {
    // 停定时器 → 装载倒计时 → 清 TF/AF + 使能 TIE → 启动（TE=1，TD=1Hz）
    if i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x00]).await.is_err() {
        return false;
    }
    if i2c_bus::write(ADDR, &[REG_TIMER_A, count]).await.is_err() {
        return false;
    }
    if i2c_bus::write(ADDR, &[REG_CTRL2, 0x01]).await.is_err() {
        return false;
    }
    i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x82]).await.is_ok()
}

/// 关闭倒计时定时器（唤醒后调用：TE=0 后 INT 不再拉低 GPIO4，
/// 防止运行期把按键网络拉低造成假按键）。
pub async fn disarm_timer() -> bool {
    i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x00]).await.is_ok()
}

/// 深睡唤醒后调用：读 TF 判断是否定时唤醒，并清标志 + 关定时器。
/// true = PCF 定时唤醒；false = 按钮唤醒。
pub async fn consume_timer_wake() -> bool {
    let mut b = [0u8; 1];
    let ok = i2c_bus::write_read(ADDR, &[REG_CTRL2], &mut b).await.is_ok();
    // 清 TF/AF（写 0）+ 停定时器 + 关中断使能——报警布防遗留的 AIE 一并关闭，
    // 否则运行期到点 INT 拉低 GPIO4 造成假按键
    let _ = i2c_bus::write(ADDR, &[REG_CTRL2, 0x00]).await;
    let _ = i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x00]).await;
    ok && (b[0] & (0x04 | 0x08) != 0) // TF（倒计时）或 AF（报警）
}

/// 深睡前布防绝对时间报警（PCF 自身走时，UTC 秒）：到点 INT 拉低唤醒。
/// 用于超过倒计时上限（255 分钟）的长间隔——纽扣电池 32k 晶振走时精确，
/// 主控深睡慢时钟漂移大。报警无月份寄存器：间隔 >28 天时会在次月同日
/// 提前唤醒一次（醒来重新布防即可，无害）。
pub async fn arm_alarm_at(ts: i64) -> bool {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(ts) else {
        return false;
    };
    // 停中断源并清 TF/AF（遗留标志会导致立即唤醒）
    if i2c_bus::write(ADDR, &[REG_CTRL2, 0x00]).await.is_err() {
        return false;
    }
    if i2c_bus::write(ADDR, &[REG_TIMER_CTRL, 0x00]).await.is_err() {
        return false;
    }
    // 报警寄存器 0x09 起自动连写 0x0A~0x0C：分/时/日 BCD 比较使能，
    // 星期寄存器写 0x80（bit7=1 禁用比较）
    let ok = i2c_bus::write(
        ADDR,
        &[
            REG_ALARM_MIN,
            to_bcd(dt.minute() as u8),
            to_bcd(dt.hour() as u8),
            to_bcd(dt.day() as u8),
            0x80,
        ],
    )
    .await
    .is_ok();
    // 使能报警中断 AIE（bit1）
    i2c_bus::write(ADDR, &[REG_CTRL2, 0x02]).await.is_ok() && ok
}
