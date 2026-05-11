use core::str::FromStr;
use esp_println::println;
use heapless::{String, Vec};
use mini_json::parse_json as json_parse;
use crate::model::seniverse::{DailyResult, Daily, Location};

fn wmo_to_seniverse_code(wmo: u8) -> &'static str {
    match wmo {
        0 => "0",
        1 => "1",
        2 => "4",
        3 => "9",
        45 | 48 => "30",
        51 | 53 => "13",
        55 => "14",
        56 | 57 => "19",
        61 => "13",
        63 => "14",
        65 => "15",
        66 | 67 => "19",
        71 | 77 | 85 => "22",
        73 => "23",
        75 | 86 => "24",
        80 => "10",
        81 => "14",
        82 => "16",
        95 => "11",
        96 | 99 => "12",
        _ => "99",
    }
}

fn wmo_to_chinese(wmo: u8) -> &'static str {
    match wmo {
        0 => "晴",
        1 => "晴间多云",
        2 => "多云",
        3 => "阴",
        45 | 48 => "雾",
        51 => "小毛毛雨",
        53 => "毛毛雨",
        55 => "大毛毛雨",
        56 | 57 => "冻雨",
        61 => "小雨",
        63 => "中雨",
        65 => "大雨",
        66 | 67 => "冻雨",
        71 => "小雪",
        73 => "中雪",
        75 => "大雪",
        77 => "米雪",
        80 => "阵雨",
        81 => "中阵雨",
        82 => "大阵雨",
        85 => "阵雪",
        86 => "大阵雪",
        95 | 96 | 99 => "雷阵雨",
        _ => "未知",
    }
}

fn wind_deg_to_chinese(deg: f64) -> &'static str {
    let d = deg as i32 % 360;
    match d {
        0..=11 | 349..=360 => "北",
        12..=33 => "北东北",
        34..=56 => "东北",
        57..=78 => "东东北",
        79..=101 => "东",
        102..=123 => "东东南",
        124..=146 => "东南",
        147..=168 => "南东南",
        169..=191 => "南",
        192..=213 => "南西南",
        214..=236 => "西南",
        237..=258 => "西西南",
        259..=281 => "西",
        282..=303 => "西西北",
        304..=326 => "西北",
        327..=348 => "北西北",
        _ => "北",
    }
}

fn kmh_to_beaufort(kmh: f64) -> &'static str {
    match kmh {
        x if x < 1.0 => "0",
        x if x < 6.0 => "1",
        x if x < 12.0 => "2",
        x if x < 20.0 => "3",
        x if x < 29.0 => "4",
        x if x < 39.0 => "5",
        x if x < 50.0 => "6",
        x if x < 62.0 => "7",
        x if x < 75.0 => "8",
        x if x < 89.0 => "9",
        x if x < 103.0 => "10",
        x if x < 117.0 => "11",
        _ => "12",
    }
}

fn f64_to_int_string(v: f64) -> String<20> {
    // manual round: add 0.5 for positive, subtract for negative
    let n = if v >= 0.0 { (v + 0.5) as i32 } else { (v - 0.5) as i32 };
    int_to_string(n)
}

fn int_to_string(mut n: i32) -> String<20> {
    if n == 0 {
        return String::from_str("0").unwrap_or_default();
    }
    let mut buf = [0u8; 12];
    let mut pos = 12;
    let negative = n < 0;
    if negative { n = -n; }
    while n > 0 {
        pos -= 1;
        buf[pos] = (n % 10) as u8 + b'0';
        n /= 10;
    }
    if negative {
        pos -= 1;
        buf[pos] = b'-';
    }
    core::str::from_utf8(&buf[pos..])
        .unwrap_or("0")
        .chars()
        .collect()
}

fn f64_to_string(v: f64) -> String<20> {
    // Format as integer + one decimal place
    let int_part = if v >= 0.0 { v as i32 } else { -( (-v) as i32) };
    let abs_v = if v >= 0.0 { v } else { -v };
    let frac = ((abs_v - (abs_v as i32) as f64) * 10.0) as i32;
    let frac = if frac < 0 { 0 } else { frac };

    let mut s = int_to_string(int_part);
    s.push('.').ok();
    s.push((b'0' + frac as u8) as char).ok();
    s
}

macro_rules! get_daily_array {
    ($daily_obj:expr, $key:literal) => {
        $daily_obj.get($key).and_then(|v| v.get_array())
    };
}

macro_rules! get_number_at {
    ($arr:expr, $idx:expr) => {
        $arr.get($idx).and_then(|v| v.get_number())
    };
}

macro_rules! get_string_at {
    ($arr:expr, $idx:expr) => {
        $arr.get($idx).and_then(|v| v.get_string())
    };
}

pub fn parse_json(data: &[u8]) -> Option<DailyResult> {
    let json_str = core::str::from_utf8(data).ok()?;
    let json = json_parse(json_str).ok()?;

    let daily_obj = json.get("daily")?;

    let time_arr = get_daily_array!(daily_obj, "time")?;
    let day_count = time_arr.len().min(5);

    let weather_code_arr = get_daily_array!(daily_obj, "weather_code");
    let temp_max_arr = get_daily_array!(daily_obj, "temperature_2m_max");
    let temp_min_arr = get_daily_array!(daily_obj, "temperature_2m_min");
    let precip_arr = get_daily_array!(daily_obj, "precipitation_sum");
    let humidity_arr = get_daily_array!(daily_obj, "relative_humidity_2m_mean");
    let wind_speed_arr = get_daily_array!(daily_obj, "wind_speed_10m_max");
    let wind_dir_arr = get_daily_array!(daily_obj, "wind_direction_10m_dominant");

    let mut daily: Vec<Daily, 5> = Vec::new();

    for i in 0..day_count {
        let mut d = Daily::default();

        // date
        if let Some(s) = get_string_at!(time_arr, i) {
            d.date = s.chars().collect();
        }

        // weather code
        let wmo_code = weather_code_arr
            .and_then(|arr| get_number_at!(arr, i))
            .unwrap_or(0.0) as u8;

        d.code_day = String::from_str(wmo_to_seniverse_code(wmo_code)).unwrap_or_default();
        d.code_night = d.code_day.clone();
        d.text_day = String::from_str(wmo_to_chinese(wmo_code)).unwrap_or_default();
        d.text_night = d.text_day.clone();

        // temperatures
        if let Some(hi) = temp_max_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.high = f64_to_int_string(hi);
        }
        if let Some(lo) = temp_min_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.low = f64_to_int_string(lo);
        }

        // precipitation
        if let Some(p) = precip_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.rainfall = f64_to_string(p);
            d.precip = d.rainfall.clone();
        }

        // humidity
        if let Some(h) = humidity_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.humidity = f64_to_int_string(h);
        }

        // wind
        if let Some(ws) = wind_speed_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.wind_speed = f64_to_string(ws);
            d.wind_scale = String::from_str(kmh_to_beaufort(ws)).unwrap_or_default();
        }
        if let Some(wd) = wind_dir_arr.and_then(|arr| get_number_at!(arr, i)) {
            d.wind_direction = String::from_str(wind_deg_to_chinese(wd)).unwrap_or_default();
            d.wind_direction_degree = f64_to_string(wd);
        }

        daily.push(d).ok();
    }

    let mut location = Location::default();
    location.name = String::from_str("Open-Meteo").unwrap_or_default();
    if let Some(tz) = json.get("timezone").and_then(|v| v.get_string()) {
        location.timezone = tz.chars().collect();
    }

    let last_update = if let Some(t) = time_arr.get(0).and_then(|v| v.get_string()) {
        let mut s: String<40> = String::from_str(t).unwrap_or_default();
        s.push('T').ok();
        s.push_str("00:00").ok();
        s
    } else {
        String::new()
    };

    println!("Open-Meteo parsed {} days", daily.len());
    Some(DailyResult {
        last_update,
        daily,
        location,
    })
}
