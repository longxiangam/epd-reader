use esp_println::println;
use heapless::Vec;
use mini_json::parse_json;

#[derive(Debug, Default)]
pub struct Holiday {
    pub date: u32, 
    pub is_off_day: bool,
}

#[derive(Debug, Default)]
pub struct HolidayResponse {
    pub year: u32,
    pub holidays: Vec<Holiday, 40>,
}

//json解析堆不够用
pub fn form_json(str: &[u8]) -> Option<HolidayResponse> {

    let result = parse_json(core::str::from_utf8(str).unwrap());
    match result {
        Ok(json_data) => {
            let mut holidays: Vec<Holiday, 40> = Vec::new();
            let obj = json_data.get_object();
            if obj.is_none() {
                return None;
            }
            for (date_key, value) in obj.unwrap().iter() {
                let mut temp = Holiday::default();
                // 将日期字符串先将 '-' 替换为空，再去掉前4位年份，只保留 MMdd
                let mut date_str: heapless::String<8> = heapless::String::new();
                for c in date_key.chars() {
                    if c != '-' {
                        date_str.push(c).ok();
                    }
                }
                let yyyymmdd: &str = &date_str;
                let date_num: u32 = yyyymmdd.parse().unwrap_or(0);
                temp.date = date_num;
                // 只解析 isOffDay 字段
                if let Some(holiday_obj) = value.get_object() {
                    for (k, v) in holiday_obj.iter() {
                        if k == "isOffDay" {
                            temp.is_off_day = v.get_bool().unwrap_or(false);
                        }
                    }
                }
                holidays.push(temp);
            }
            let resp = HolidayResponse { year:0, holidays };
            println!("holiday format: {:?}", resp);
            Some(resp)
        }
        Err(e) => {
            println!("holiday json error: {:?}", e);
            None
        }
    }
}

//遍历对象解析（适配 holiday-cn 格式：days 数组，每项含 date + isOffDay）
pub fn form_json_each(str: &[u8]) -> Option<HolidayResponse> {
    let s = core::str::from_utf8(str).unwrap_or("");
    let mut holidays: Vec<Holiday, 40> = Vec::new();
    let mut year: u32 = 0;

    // 解析年份：找 "year": NNNN
    if let Some(pos) = s.find("\"year\"") {
        let after = &s[pos..];
        if let Some(colon) = after.find(':') {
            let rest = &after[colon + 1..];
            let rest = rest.trim_start();
            let digits: heapless::String<8> = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(y) = digits.parse::<u32>() {
                year = y;
            }
        }
    }
    if year == 0 {
        return None;
    }

    // 扫描所有 "date": "YYYY-MM-DD"，然后找最近的 "isOffDay": true/false
    let mut search_from = 0;
    while let Some(date_pos) = s[search_from..].find("\"date\"") {
        let date_pos = search_from + date_pos;
        // 找 date 值
        if let Some(val_start) = s[date_pos..].find("\"20") {
            let val_start = date_pos + val_start + 1;
            if let Some(val_end) = s[val_start..].find("\"") {
                let date_str = &s[val_start..val_start + val_end];
                if date_str.len() == 10 && date_str.chars().filter(|c| *c == '-').count() == 2 {
                    // 解析 YYYYMMDD
                    let yyyymmdd: heapless::String<8> = date_str.chars().filter(|c| *c != '-').collect();
                    let date_num: u32 = yyyymmdd.parse().unwrap_or(0);

                    // 在 date 之后找 "isOffDay"（不限制字节数，避免 UTF-8 边界 panic）
                    let region = &s[val_start + val_end..];
                    let is_off = region.contains("\"isOffDay\":true") || region.contains("\"isOffDay\": true");

                    holidays.push(Holiday { date: date_num, is_off_day: is_off }).ok();
                    search_from = val_start + val_end;
                    continue;
                }
            }
        }
        search_from = date_pos + 6;
    }

    if holidays.is_empty() {
        None
    } else {
        let resp = HolidayResponse { year, holidays };
        Some(resp)
    }
}

fn parse_single_holiday(date_obj: &str, date_key: &str) -> Option<Holiday> {
    let mut holiday = Holiday::default();
   // 将日期字符串先将 '-' 替换为空，再去掉前4位年份，只保留 MMdd
   let mut date_str: heapless::String<8> = heapless::String::new();
   for c in date_key.chars() {
       if c != '-' {
           date_str.push(c).ok();
       }
   }
   let yyyymmdd: &str = &date_str;
   let date_num: u32 = yyyymmdd.parse().unwrap_or(0);
   holiday.date = date_num;

    // 使用parse_json解析日期对象
    match parse_json(core::str::from_utf8(date_obj.as_bytes()).unwrap()) {
        Ok(json_data) => {
            if let Some(obj) = json_data.get_object() {
                for (key, value) in obj.iter() {
                    if key == "isOffDay" {
                        holiday.is_off_day = value.get_bool().unwrap_or(false);
                        break;
                    }
                }
            }
            Some(holiday)
        }
        Err(_) => None
    }
}