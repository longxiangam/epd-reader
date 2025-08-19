/** 阳历1900.1.31＝阴历0.1.1
 * 每个32位数据各代表一年，其中bit0 - bit16共17位有意义。
bit [3:0]	数值范围为0 - C 。0 代表此年不存在闰月，非零表示此年有闰月，具体数值 1 - C 表示闰月月份
bit [15:4]	bit 15 - bit 4 分别代表此年的1 - 12月，为 0 表示此月为小月（29日），为 1 表示大月（30日）
bit16	在bit [3:0] 为 0 时忽略；其值仅在bit [3:0]不为 0 时有效，为1时表示该年闰月为大月，为 0 时表小月
 * 
 * 使用示例:
 * ```rust
 * use crate::model::lunar::Lunar;
 * 
 * // 创建2024年1月的阴历转换器
 * let lunar = Lunar::new(2024, 1);
 * 
 * // 获取1月1日的阴历信息
 * if let Some(lunar_day) = lunar.get_lunar_day(1) {
 *     println!("阴历月份: {}", lunar_day.get_month_name());
 *     println!("阴历日期: {}", lunar_day.get_day_name());
 * }
 * 
 * // 获取所有阴历日期
 * let all_days = lunar.get_all_lunar_days();
 * ```
 */
use esp_println::{print, println};

const LUNAR_DATA:[u32; 201 ] = [

    0x04bd8, 0x04ae0, 0x0a570, 0x054d5, 0x0d260, 0x0d950, 0x16554, 0x056a0, 0x09ad0, 0x055d2, //1900-1909
    0x04ae0, 0x0a5b6, 0x0a4d0, 0x0d250, 0x1d255, 0x0b540, 0x0d6a0, 0x0ada2, 0x095b0, 0x14977, //1910-1919
    0x04970, 0x0a4b0, 0x0b4b5, 0x06a50, 0x06d40, 0x1ab54, 0x02b60, 0x09570, 0x052f2, 0x04970, //1920-1929
    0x06566, 0x0d4a0, 0x0ea50, 0x16a95, 0x05ad0, 0x02b60, 0x186e3, 0x092e0, 0x1c8d7, 0x0c950, //1930-1939
    0x0d4a0, 0x1d8a6, 0x0b550, 0x056a0, 0x1a5b4, 0x025d0, 0x092d0, 0x0d2b2, 0x0a950, 0x0b557, //1940-1949
    0x06ca0, 0x0b550, 0x15355, 0x04da0, 0x0a5b0, 0x14573, 0x052b0, 0x0a9a8, 0x0e950, 0x06aa0, //1950-1959
    0x0aea6, 0x0ab50, 0x04b60, 0x0aae4, 0x0a570, 0x05260, 0x0f263, 0x0d950, 0x05b57, 0x056a0, //1960-1969
    0x096d0, 0x04dd5, 0x04ad0, 0x0a4d0, 0x0d4d4, 0x0d250, 0x0d558, 0x0b540, 0x0b6a0, 0x195a6, //1970-1979
    0x095b0, 0x049b0, 0x0a974, 0x0a4b0, 0x0b27a, 0x06a50, 0x06d40, 0x0af46, 0x0ab60, 0x09570, //1980-1989
    0x04af5, 0x04970, 0x064b0, 0x074a3, 0x0ea50, 0x06b58, 0x05ac0, 0x0ab60, 0x096d5, 0x092e0, //1990-1999
    0x0c960, 0x0d954, 0x0d4a0, 0x0da50, 0x07552, 0x056a0, 0x0abb7, 0x025d0, 0x092d0, 0x0cab5, //2000-2009
    0x0a950, 0x0b4a0, 0x0baa4, 0x0ad50, 0x055d9, 0x04ba0, 0x0a5b0, 0x15176, 0x052b0, 0x0a930, //2010-2019
    0x07954, 0x06aa0, 0x0ad50, 0x05b52, 0x04b60, 0x0a6e6, 0x0a4e0, 0x0d260, 0x0ea65, 0x0d530, //2020-2029
    0x05aa0, 0x076a3, 0x096d0, 0x026fb, 0x04ad0, 0x0a4d0, 0x1d0b6, 0x0d250, 0x0d520, 0x0dd45, //2030-2039       //2033有误
    0x0b5a0, 0x056d0, 0x055b2, 0x049b0, 0x0a577, 0x0a4b0, 0x0aa50, 0x1b255, 0x06d20, 0x0ada0, //2040-2049
    0x14b63, 0x09370, 0x049f8, 0x04970, 0x064b0, 0x168a6, 0x0ea50, 0x06aa0, 0x1a6c4, 0x0aae0, //2050-2059
    0x092e0, 0x0d2e3, 0x0c960, 0x0d557, 0x0d4a0, 0x0da50, 0x05d55, 0x056a0, 0x0a6d0, 0x055d4, //2060-2069
    0x052d0, 0x0a9b8, 0x0a950, 0x0b4a0, 0x0b6a6, 0x0ad50, 0x055a0, 0x0aba4, 0x0a5b0, 0x052b0, //2070-2079
    0x0b273, 0x06930, 0x07337, 0x06aa0, 0x0ad50, 0x14b55, 0x04b60, 0x0a570, 0x054e4, 0x0d160, //2080-2089
    0x0e968, 0x0d520, 0x0daa0, 0x16aa6, 0x056d0, 0x04ae0, 0x0a9d4, 0x0a2d0, 0x0d150, 0x0f252, //2090-2099
    0x0d520                                                                                   //2100

];

//阳历指定月份的农历
pub struct Lunar{
    solar_year:u16,
    solar_month:u8,
    days:[Option<LunarDay>;31],

}
#[derive(Debug,Default,Copy,Clone)]
pub struct LunarDay{
    year:u16,
    month:u8,
    day:u16,
    leap_month:bool,
}

impl LunarDay {
    /// 获取阴历年份
    pub fn get_year(&self) -> u16 {
        self.year
    }
    
    /// 获取阴历月份
    pub fn get_month(&self) -> u8 {
        self.month
    }
    
    /// 获取阴历日期
    pub fn get_day(&self) -> u16 {
        self.day
    }
    
    /// 是否为闰月
    pub fn is_leap_month(&self) -> bool {
        self.leap_month
    }
    
    /// 获取阴历月份名称
    pub fn get_month_name(&self) -> &'static str {
        if self.leap_month {
            match self.month {
                1 => "闰正月",
                2 => "闰二月",
                3 => "闰三月",
                4 => "闰四月",
                5 => "闰五月",
                6 => "闰六月",
                7 => "闰七月",
                8 => "闰八月",
                9 => "闰九月",
                10 => "闰十月",
                11 => "闰冬月",
                12 => "闰腊月",
                _ => "未知",
            }
        } else {
            match self.month {
                1 => "正月",
                2 => "二月",
                3 => "三月",
                4 => "四月",
                5 => "五月",
                6 => "六月",
                7 => "七月",
                8 => "八月",
                9 => "九月",
                10 => "十月",
                11 => "冬月",
                12 => "腊月",
                _ => "未知",
            }
        }
    }
    
    /// 获取阴历日期名称
    pub fn get_day_name(&self) -> &'static str {
        match self.day {
            1 => "初一",
            2 => "初二",
            3 => "初三",
            4 => "初四",
            5 => "初五",
            6 => "初六",
            7 => "初七",
            8 => "初八",
            9 => "初九",
            10 => "初十",
            11 => "十一",
            12 => "十二",
            13 => "十三",
            14 => "十四",
            15 => "十五",
            16 => "十六",
            17 => "十七",
            18 => "十八",
            19 => "十九",
            20 => "二十",
            21 => "廿一",
            22 => "廿二",
            23 => "廿三",
            24 => "廿四",
            25 => "廿五",
            26 => "廿六",
            27 => "廿七",
            28 => "廿八",
            29 => "廿九",
            30 => "三十",
            _ => "未知",
        }
    }
   
}
 
impl Lunar {
    pub fn new(solar_year: u16, solar_month: u8) -> Self {
        let mut lunar = Lunar {
            solar_year,
            solar_month,
            days: [None; 31],
        };
        
        // 计算农历
        lunar.calculate_lunar_days();
        lunar
    }
    
    /// 计算指定阳历月份每天对应的阴历日期
    fn calculate_lunar_days(&mut self) {
        // 计算阳历月份的天数
        let solar_month_days = self.get_solar_month_days(self.solar_year, self.solar_month);
        
        // 为每一天计算对应的阴历日期
        for day in 1..=solar_month_days {
            let lunar_date = self.calculate_lunar_date_from_solar(self.solar_year, self.solar_month, day);
            self.days[(day - 1) as usize] = Some(lunar_date);
        }
    }
    
    /// 计算指定阳历月份的天数
    fn get_solar_month_days(&self, year: u16, month: u8) -> u8 {
        match month {
            2 => {
                if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                    29 // 闰年2月
                } else {
                    28 // 平年2月
                }
            }
            4 | 6 | 9 | 11 => 30, // 小月
            _ => 31, // 大月
        }
    }
    
    /// 从阳历日期计算阴历日期
    fn calculate_lunar_date_from_solar(&self, solar_year: u16, solar_month: u8, solar_day: u8) -> LunarDay {
        // 计算从1900年1月31日到指定阳历日期的总天数
        let days_since_1900 = self.calculate_days_since_1900(solar_year, solar_month, solar_day);
        
        // 从阴历0年开始累加阴历天数，找到对应的阴历日期
        let mut lunar_year = 0;
        let mut remaining_days = days_since_1900;
        
        // 循环计算阴历年份
        loop {
            let year_days = self.calculate_lunar_year_days(lunar_year);
            if remaining_days < year_days {
                break;
            }
            remaining_days -= year_days;
            lunar_year += 1;
        }
        
        // 计算阴历月份和日期
        let (lunar_month, lunar_day, is_leap) = self.calculate_lunar_month_day(lunar_year, remaining_days);
        
        LunarDay {
            year: lunar_year,
            month: lunar_month,
            day: lunar_day,
            leap_month: is_leap,
        }
    }
    
    /// 计算从1900年1月31日到指定阳历日期的总天数
    fn calculate_days_since_1900(&self, year: u16, month: u8, day: u8) -> u32 {
        let mut total_days = 0;
        
        // 计算从1900年到指定年份前一年的天数
        for y in 1900..year {
            total_days += if self.is_leap_year(y) { 366 } else { 365 };
        }
        
        // 计算指定年份从1月1日到指定月份前一个月的天数
        for m in 1..month {
            total_days += self.get_solar_month_days(year, m) as u32;
        }
        
        // 加上指定月份的天数，减去1月31日的偏移（因为1900.1.31是阴历0.1.1）
        total_days += day as u32 - 31;
        
        total_days
    }
    
    /// 判断是否为闰年
    fn is_leap_year(&self, year: u16) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }
    
    /// 计算阴历月份和日期
    fn calculate_lunar_month_day(&self, lunar_year: u16, remaining_days: u32) -> (u8, u16, bool) {
        let year_index = lunar_year as usize;
        if year_index >= LUNAR_DATA.len() {
            return (1, 1, false); // 超出数据范围，返回默认值
        }
        
        let lunar_data = LUNAR_DATA[year_index];
        let leap_month = (lunar_data & 0xF) as u8;
        let is_leap_month_big = (lunar_data >> 16) & 1 == 1;
        let month_days = (lunar_data >> 4) & 0xFFF;
        
        let mut lunar_month = 1;
        let mut is_leap = false;
        let mut remaining_days = remaining_days;
        
        // 遍历12个月，正确处理闰月
        let mut month_count = 0;
        
        for month in 1..=12u8 {
            // 先处理正常月份
            let shift = (12 - month) as u32;
            let is_big_month = (month_days >> shift) & 1 == 1;
            let month_length = if is_big_month { 30 } else { 29 };
            
            if remaining_days < month_length {
                lunar_month = month;
                break;
            }
            remaining_days -= month_length;
            month_count += 1;
            
            // 如果这个月有闰月，在正常月份之后插入闰月
            if month == leap_month && leap_month > 0 {
                let leap_month_length = if is_leap_month_big { 30 } else { 29 };
                if remaining_days < leap_month_length {
                    lunar_month = month;
                    is_leap = true;
                    break;
                }
                remaining_days -= leap_month_length;
                month_count += 1;
            }
        }
        
        // 计算阴历日期（从1开始）
        let lunar_day = (remaining_days + 1) as u16;
        
        (lunar_month, lunar_day, is_leap)
    }
    
    /// 计算指定阴历年份的总天数
    fn calculate_lunar_year_days(&self, lunar_year: u16) -> u32 {
        let year_index = lunar_year as usize;
        if year_index >= LUNAR_DATA.len() {
            return 0;
        }
        
        let lunar_data = LUNAR_DATA[year_index];
        let leap_month = (lunar_data & 0xF) as u8;
        let is_leap_month_big = (lunar_data >> 16) & 1 == 1;
        let month_days = (lunar_data >> 4) & 0xFFF;
        
        let mut total_days = 0;
        
        // 计算12个月的天数
        for month in 1..=12u8 {
            let shift = (12 - month) as u32;
            let is_big_month = (month_days >> shift) & 1 == 1;
            total_days += if is_big_month { 30 } else { 29 };
        }
        
        // 如果有闰月，加上闰月的天数
        if leap_month > 0 {
            total_days += if is_leap_month_big { 30 } else { 29 };
        }
        
        total_days
    }
    
    /// 获取指定日期的阴历信息
    pub fn get_lunar_day(&self, day: u8) -> Option<LunarDay> {
        if day > 0 && day <= 31 {
            self.days[(day - 1) as usize]
        } else {
            None
        }
    }
    
    /// 获取阳历年份
    pub fn get_solar_year(&self) -> u16 {
        self.solar_year
    }
    
    /// 获取阳历月份
    pub fn get_solar_month(&self) -> u8 {
        self.solar_month
    }
    
    /// 获取所有阴历日期
    pub fn get_all_lunar_days(&self) -> &[Option<LunarDay>; 31] {
        &self.days
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_2025_august_1() {
        // 测试2025年8月1日的阴历计算
        // 应该输出闰六月初八
        let lunar = Lunar::new(2025, 8);
        
        // 添加调试信息
        println!("2025年的阴历数据: 0x0a6e6");
        println!("bit [3:0] = 6 (闰六月)");
        println!("bit [15:4] = 1010 0110 1110 (1-12月的天数)");
        println!("bit 16 = 0 (闰六月是小月，29天)");
        
        if let Some(lunar_day) = lunar.get_lunar_day(1) {
            println!("2025年8月1日对应的阴历: {}{}", 
                lunar_day.get_month_name(), lunar_day.get_day_name());
            println!("阴历年: {}, 月: {}, 日: {}, 闰月: {}", 
                lunar_day.get_year(), lunar_day.get_month(), lunar_day.get_day(), lunar_day.is_leap_month());
            
            // 验证应该是闰六月初八
            assert_eq!(lunar_day.get_month(), 6);
            assert_eq!(lunar_day.get_day(), 8);
            assert_eq!(lunar_day.is_leap_month(), true);
        } else {
            panic!("无法获取2025年8月1日的阴历信息");
        }
    }
    
    #[test]
    fn test_lunar_calculation() {
        // 测试2024年1月的阴历计算
        let lunar = Lunar::new(2024, 1);
        
        // 验证基本属性
        assert_eq!(lunar.get_solar_year(), 2024);
        assert_eq!(lunar.get_solar_month(), 1);
        
        // 获取1月1日的阴历信息
        if let Some(lunar_day) = lunar.get_lunar_day(1) {
            println!("2024年1月1日: {}", lunar_day.get_month_name());
            println!("阴历日期: {}", lunar_day.get_day_name());
            println!("阴历年: {}, 月: {}, 日: {}, 闰月: {}", 
                lunar_day.get_year(), lunar_day.get_month(), lunar_day.get_day(), lunar_day.is_leap_month());
        }
    }
    
    #[test]
    fn test_days_since_1900() {
        let lunar = Lunar::new(2024, 1);
        
        // 测试从1900年1月31日到2024年1月1日的天数计算
        // 2024年1月1日应该是阴历2023年11月20日左右
        if let Some(lunar_day) = lunar.get_lunar_day(1) {
            println!("2024年1月1日对应的阴历: {}{}", 
                lunar_day.get_month_name(), lunar_day.get_day_name());
        }
    }
    
    #[test]
    fn test_lunar_day_methods() {
        let lunar_day = LunarDay {
            year: 2024,
            month: 1,
            day: 15,
            leap_month: false,
        };
        
        assert_eq!(lunar_day.get_year(), 2024);
        assert_eq!(lunar_day.get_month(), 1);
        assert_eq!(lunar_day.get_day(), 15);
        assert_eq!(lunar_day.is_leap_month(), false);
        assert_eq!(lunar_day.get_month_name(), "正月");
        assert_eq!(lunar_day.get_day_name(), "十五");
    }
}
