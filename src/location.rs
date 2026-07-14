use core::str::FromStr;
use heapless::String;
use mini_json::parse_json;
use esp_println::println;

use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};

const LOCATE_URL: &str =
    "http://ip-api.com/json/?fields=status,lat,lon,city&lang=zh-CN";

pub struct LocateResult {
    pub city: String<32>,
    pub latlon: String<32>,
}

/// 通过 IP 接口定位当前经纬度与城市，返回 "lat:lon"（心知天气 location 参数可直接使用）
pub async fn locate() -> Option<LocateResult> {
    println!("start locate");
    let stack = match use_wifi().await {
        Ok(s) => s,
        Err(e) => {
            println!("locate use_wifi failed: {:?}", e);
            return None;
        }
    };
    let mut request = RequestClient::new(stack).await;
    let result = request.send_request(LOCATE_URL).await;
    finish_wifi().await;

    let response = result.ok()?;
    let json_str = core::str::from_utf8(&response.data[..response.length]).ok()?;
    println!("locate response: {}", json_str);

    let json = match parse_json(json_str) {
        Ok(j) => j,
        Err(e) => {
            println!("locate parse failed: {:?}", e);
            return None;
        }
    };

    let status_ok = json
        .get("status")
        .and_then(|v| v.get_string())
        .map(|s| s.as_str() == "success")
        .unwrap_or(true);
    if !status_ok {
        println!("locate failed status");
        return None;
    }

    let lat = json.get("lat").and_then(|v| v.get_number())?;
    let lon = json.get("lon").and_then(|v| v.get_number())?;
    let city_raw = json
        .get("city")
        .and_then(|v| v.get_string())
        .map(|s| s.as_str())
        .unwrap_or("");

    let mut latlon: String<32> = String::new();
    latlon.push_str(&format_coord(lat)).ok();
    latlon.push(':').ok();
    latlon.push_str(&format_coord(lon)).ok();

    let city: String<32> = city_raw.chars().collect();
    println!("located: {} {}", city, latlon);
    Some(LocateResult { city, latlon })
}

/// 将坐标格式化为 4 位小数的字符串
fn format_coord(v: f64) -> String<12> {
    let neg = v < 0.0;
    let abs = if neg { -v } else { v };
    let int_part = abs as u32;
    let mut frac = ((abs - int_part as f64) * 10000.0) as u32;
    if frac > 9999 {
        frac = 9999;
    }

    let mut s: String<12> = String::new();
    if neg {
        s.push('-').ok();
    }
    s.push_str(&uint_to_string(int_part)).ok();
    s.push('.').ok();
    let fs = uint_to_string(frac);
    for _ in 0..4usize.saturating_sub(fs.len()) {
        s.push('0').ok();
    }
    s.push_str(&fs).ok();
    s
}

fn uint_to_string(mut n: u32) -> String<12> {
    if n == 0 {
        return String::from_str("0").unwrap();
    }
    let mut tmp = [0u8; 10];
    let mut pos = 10;
    while n > 0 {
        pos -= 1;
        tmp[pos] = (n % 10) as u8 + b'0';
        n /= 10;
    }
    let mut s: String<12> = String::new();
    for i in pos..10 {
        s.push(tmp[i] as char).ok();
    }
    s
}
