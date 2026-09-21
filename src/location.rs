//! IP 定位（设置页自动定位）。
//!
//! 服务：myip.ipip.net/json（国内 CDN，UTF-8，免 key）。
//! 响应：{"ret":"ok","data":{"ip":"...","location":["中国","湖北","武汉","","移动"]}}
//! location[2] 即城市名（心知天气 location 参数可直接使用城市名）。
//! 旧源 ip-api.com 为境外服务，国内直连 TCP 常超时，故弃用。

use heapless::String;

use crate::request::RequestClient;
use crate::wifi::{finish_wifi, use_wifi};
use esp_println::println;

const LOCATE_URL: &str = "https://myip.ipip.net/json";

pub struct LocateResult {
    pub city: String<32>,
    /// 定位结果的位置参数（心知天气接受城市名；无坐标时留空）
    pub latlon: String<32>,
}

/// 通过 IP 接口定位当前城市
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

    // location 数组：["国家","省","市","区县","运营商"] → 取第 3 个字段（城市）
    let loc_pos = json_str.find("\"location\"")?;
    let arr_pos = json_str[loc_pos..].find('[')? + loc_pos;
    let rest = &json_str[arr_pos + 1..];

    let mut fields: [&str; 3] = ["", "", ""];
    let mut pos = 0usize;
    for field in fields.iter_mut() {
        let open = rest[pos..].find('"')? + pos;
        let close = rest[open + 1..].find('"')? + open + 1;
        *field = &rest[open + 1..close];
        pos = close + 1;
    }
    let city_raw = fields[2];
    if city_raw.is_empty() {
        println!("locate city empty");
        return None;
    }

    let mut city: String<32> = String::new();
    city.push_str(city_raw).ok();
    // 国内 IP 接口无经纬度：位置参数直接用城市名（心知天气支持）
    let mut latlon: String<32> = String::new();
    latlon.push_str(city_raw).ok();
    println!("located: {}", city);
    Some(LocateResult { city, latlon })
}
