
use core::str::FromStr;
use esp_println::println;
use heapless::{String, Vec};
use mini_json::parse_json;

//三日
#[derive( Debug)]
pub struct  DailyResponse{
    pub results:Vec<DailyResult,1>
}
#[derive( Debug,Default)]
pub struct  DailyResult{
    pub last_update:String<40>,
    pub daily:Vec<Daily,5>,
    pub location: Location
}
#[derive( Debug,Default)]
pub struct Location{
    pub id: String<20>,
    pub name: String<20>,
    pub country: String<20>,
    pub path:  String<50>,
    pub timezone: String<30>,
    pub timezone_offset: String<20>
}
#[derive( Debug,Default)]
pub struct Daily{
    pub  date:String<20>,
    pub  text_day: String<20>,
    pub code_day: String<20>,
    pub text_night:String<20>,
    pub  code_night:String<20>,
    pub high: String<20>,
    pub low: String<20>,
    pub rainfall:String<20>,
    pub precip: String<20>,
    pub wind_direction: String<20>,
    pub wind_direction_degree: String<20>,
    pub wind_speed: String<20>,
    pub wind_scale: String<20>,
    pub humidity: String<20>,
}
macro_rules! set_field {
    ($temp:ident, $one:ident,$key:ident, $($key_liter:literal => $field:ident),*) => {
        $(
            if $key.eq($key_liter) {
                $temp.$field = $one.1.get_string().unwrap().chars().collect();
            }
        )*
    };
}

//lite_json::parse_json 中有中文会报错
pub fn form_json(str:&[u8])->Option<DailyResponse>{
    let result = parse_json(core::str::from_utf8(str).unwrap());
    match result {
        Ok(json_data) => {

            let mut location:Option<Location> = None;
            let mut daily:Vec<Daily,5 > = Vec::new();
            let mut last_update:String<40> = String::new();

            let results = json_data.get("results");
            if let None = results {
                return None;
            }
            let temp_array = results.unwrap().get_array();
            if let None = temp_array {
                return None;
            }
            for result__ in temp_array.unwrap().iter() {
                if let Some(location_obj)  = result__.get("location"){
                    let mut temp = Location::default();

                    for one in location_obj.get_object().unwrap().iter() {
                        let key: String<40> =  one.0.chars().collect();
                        set_field!(temp,one,key
                                            ,"id"=>id
                                            ,"name"=>name
                                            ,"country"=>country
                                            ,"path"=>path
                                            ,"timezone"=>timezone
                                            ,"timezone_offset"=>timezone_offset
                                        );
                    }
                    location = Some(temp);
                }
                if let Some(daily_array)  = result__.get("daily"){
                    for one_daily in daily_array.get_array().unwrap().iter() {
                        let mut temp = Daily::default();
                        for one_daily_ in one_daily.get_object().unwrap().iter() {
                            let key: String<40> = one_daily_.0.chars().collect();
                            set_field!(temp,one_daily_,key
                                            ,"date"=>date
                                            ,"text_day"=>text_day
                                            ,"code_day"=>code_day
                                            ,"text_night"=>text_night
                                            ,"code_night"=>code_night
                                            ,"high"=>high
                                            ,"low"=>low
                                            ,"rainfall"=>rainfall
                                            ,"precip"=>precip
                                            ,"wind_direction"=>wind_direction
                                            ,"wind_direction_degree"=>wind_direction_degree
                                            ,"wind_speed"=>wind_speed
                                            ,"wind_scale"=>wind_scale
                                            ,"humidity"=>humidity
                                        );
                        }

                        daily.push(temp);
                    }
                }
                if let Some(last_update_temp)  = result__.get("last_update"){
                    last_update = last_update_temp.get_string().unwrap().chars().collect();
                }
            }

            let mut results:DailyResult = Default::default();
            if let Some(v) = location {
                results.location = v;
                results.daily = daily;
                results.last_update = last_update;

                let mut vec:Vec<DailyResult,1> = Vec::new();
                vec.push(results);
                let  daily_response = DailyResponse{ results:vec};

                println!("format:{:?}",daily_response);
                Some(daily_response)
            }else {
                None
            }
        }
        Err(e) => {
            println!("json error:{:?}",e);
            None
        }
    }
}


//实时

