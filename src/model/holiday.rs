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
