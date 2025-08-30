use core::str::FromStr;
use esp_println::println;
use heapless::{String, Vec};
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

//遍历对象解析
pub fn form_json_each(str: &[u8]) -> Option<HolidayResponse> {
    let str = core::str::from_utf8(str).unwrap();
    let mut holidays: Vec<Holiday, 40> = Vec::new();
    let mut year: u32 = 0;
    let mut remaining_str = str;
    
    //解析出年份
    if let Some(year_pos) = remaining_str.find("\"20") {
        let year_start = year_pos + 1;
        if year_start + 4 <= remaining_str.len() {
            let year_str = &remaining_str[year_start..year_start + 4];
            if let Ok(year_num) = year_str.parse::<u32>() {
                year = year_num;
                println!("year: {}", year);
            }
        }
    }
    
    if year == 0 {
        return None;
    }
    
    //从字符串中把日期取出来并把日期前的字符串截取掉
    while let Some(date_start) = remaining_str.find("\"") {
        // 找到日期字符串
        if let Some(date_end) = remaining_str[date_start + 1..].find("\"") {
            let date_end = date_start + 1 + date_end;
            let date_key = &remaining_str[date_start + 1..date_end];
            
            // 检查是否是日期格式 (YYYY-MM-DD)
            if date_key.len() == 10 && date_key.contains('-') {
                // 3. 找到第一个开始大括号和结束大括号
                if let Some(brace_start) = remaining_str[date_end..].find("{") {
                    let brace_start = date_end + brace_start;
                    if let Some(brace_end) = remaining_str[brace_start..].find("}") {
                        let brace_end = brace_start + brace_end;
                        let date_obj = &remaining_str[brace_start..=brace_end];
                        if let Some(holiday) = parse_single_holiday(date_obj, date_key) {
                            holidays.push(holiday).ok();
                        }
                        remaining_str = &remaining_str[brace_end..];
                    } else {
                        remaining_str = &remaining_str[brace_start..];
                    }
                } else {
                    remaining_str = &remaining_str[date_end..];
                }
            } else {
                remaining_str = &remaining_str[date_end..];
            }
        } else {
            break;
        }
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