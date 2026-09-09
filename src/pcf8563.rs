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
