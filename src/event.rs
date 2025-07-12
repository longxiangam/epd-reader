use alloc::boxed::Box;

use heapless::Vec;

use core::future::Future;
use core::pin::Pin;
use embassy_futures::select::{Either3, Either4, select3, select4, select};
use embassy_futures::select::Either::{First, Second};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::InputPin;
use embedded_hal_async::digital::Wait;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin};
use esp_println::println;
use futures::FutureExt;
use esp_hal::gpio::{Gpio2, GpioPin, Input, Pull};
use esp_hal::gpio::{Gpio8,Gpio9,Gpio20};
use esp_hal::peripherals::ADC1;
use futures::future::Either;
use crate::sleep::refresh_active_time;
use crate::worldtime::SntpcError::NoAddr;

#[derive(Eq, PartialEq,Debug)]
pub enum EventType{
    KeyShort(u32),
    KeyLongStart(u32),
    KeyLongIng(u32),
    KeyLongEnd(u32),
    KeyDouble(u32),
    WheelBack,
    WheelFront,
}

#[derive(Eq, PartialEq,Debug)]
pub struct EventInfo{
    pub ptr:Option<usize>,
}



///ptr 为处理对象的裸指针，因为定义的一个全局vec保存listener ，泛型不好处理，这里直接用usize
///所以要在对象drop 的同时clear 掉事件监听，不然会出现悬垂指针的问题
struct Listener{
    callback:Box< dyn FnMut(EventInfo) -> (Pin<Box< dyn Future<Output = ()>  + 'static>>)  + Send + Sync + 'static>,
    event_type:EventType,
    ptr:Option<usize>, //对象的裸指针，因为定义的一个全局vec保存listener ，泛型不好处理，这里直接用usize
    fixed:bool,//是否常驻事件
}

static LISTENER:Mutex<CriticalSectionRawMutex,Vec<Listener,20>>  = Mutex::new(Vec::new()) ;
pub async fn on<F>(event_type: EventType, callback: F)
where F: FnMut(EventInfo) -> (Pin<Box<dyn Future<Output=()>  + 'static>>) + Send + Sync + 'static,
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:None,fixed:false});
}
pub async fn on_target<F>(event_type: EventType,target_ptr:usize, callback: F)
    where F: FnMut(EventInfo) -> (Pin<Box<dyn Future<Output=()>  + 'static>>) + Send + Sync + 'static
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:Some(target_ptr),fixed:false});
}
pub async fn on_fixed<F>(event_type: EventType,target_ptr:usize, callback: F)
    where F: FnMut(EventInfo) -> (Pin<Box<dyn Future<Output=()> + 'static>>) + Send + Sync + 'static
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:Some(target_ptr),fixed:true});
}

pub async fn un(event_type: EventType)
{
    let mut vec = LISTENER.lock().await;

    let mut find_index:Option<usize> = None;
    for (index,listener) in vec.iter().enumerate() {
        if listener.event_type == event_type{
            find_index = Some(index);
        }
    }
    if let Some(v) = find_index {
        vec.remove(v);
    }
}

pub async fn clear(){
    let mut vec = LISTENER.lock().await;
    vec.clear();
}


pub async fn toggle_event(event_type: EventType,ms:u64){
    println!("event_type:{:?}",event_type);
    let mut vec = LISTENER.lock().await;
    for mut listener in vec.iter_mut() {
        if listener.event_type == event_type{

            (listener.callback)(EventInfo{ptr:listener.ptr }).await;

        }
    }

}


#[embassy_executor::task]
pub async  fn run(mut key1: Gpio9,mut key2: Gpio2){
    let mut key1 = Input::new(key1,Pull::Up);
    let mut key2 = Input::new(key2,Pull::Up);
    
    loop {

        let key1_edge = key1.wait_for_falling_edge();
        let key2_edge = key2.wait_for_falling_edge();
        match  select(key1_edge,key2_edge).await {
            First(_) => {
                key_detection::<_,1>(&mut key1).await;
            }
            Second(_) => {
                key_detection::<_,2>(&mut key2).await;
            }
        }
        refresh_active_time().await;
        Timer::after(Duration::from_millis(10)).await;
    }
}

type AdcCal = AdcCalCurve<ADC1>;
pub static ADC_PIN:Mutex<CriticalSectionRawMutex,Option<AdcPin<GpioPin<2>,ADC1,AdcCal>>> = Mutex::new(None);
pub static ADC_PER:Mutex<CriticalSectionRawMutex,Option<Adc<ADC1>>> = Mutex::new(None);

pub static  ENABLE_DOUBLE:Mutex<CriticalSectionRawMutex,bool> = Mutex::new(false);
pub async fn enable_double_click(){
    *ENABLE_DOUBLE.lock().await = true;
}
pub async fn disable_double_click(){
    *ENABLE_DOUBLE.lock().await = false;
}
pub async fn key_detection<P,const NUM:usize>(key: &mut P)
where P:InputPin
{
    let begin_ms = Instant::now().as_millis();
    let mut is_long = false;
    let mut key_num:usize = NUM;
    if NUM == 2 {
        key_num = judge_adc_num().await ;
        if key_num == 0 {
            return;
        }
    }
    
    loop {
        let mut is_low_times = 0;
        for i in 0..100 {
            if key.is_low().unwrap() {
                is_low_times += 1;
            }
        }
      
        if is_low_times > 80 {
           
            //按下
            let current = Instant::now().as_millis();
            if current - begin_ms > 500 {
                //长时间按下
                if !is_long {
                    is_long = true;
                    toggle_event(EventType::KeyLongStart(key_num as u32), current).await;
                }else {
                    toggle_event(EventType::KeyLongIng(key_num as u32), current).await;
                }
            }
        } else if is_low_times < 2 {
            //释放
            let current = Instant::now().as_millis();
            if is_long {
                //长时间按下后释放
                toggle_event(EventType::KeyLongEnd(key_num as u32), current).await;
                return;
            } else {
                //短时按下，等几ms 看是否有下一次按下，如有则是双击
                loop {
                    let current = Instant::now().as_millis();
                    if(!*ENABLE_DOUBLE.lock().await){
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    if current - begin_ms > 400 {
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    let mut is_low_times = 0;
                    for i in 0..100 {
                        if key.is_low().unwrap() {
                            is_low_times += 1;
                        }
                    }

                    //变低
                    if is_low_times > 80{
                        toggle_event(EventType::KeyDouble(key_num as u32), current).await;
                        return;
                    }
                }

            }
        }
        Timer::after(Duration::from_millis(1)).await;
    }
}

async fn judge_adc_num() -> usize{
    let mut temp = 0;
    let mut effective_num = 20;
    let mut adc_valute_sum = 0;
    while  temp == 0 &&  effective_num > 0 {
        if let Some(pin) = ADC_PIN.lock().await.as_mut() {
            if let Some(adc) = ADC_PER.lock().await.as_mut() {
                let val = adc.read_oneshot(pin);
                match val {
                    Ok(adc_value) => {
                        adc_valute_sum += adc_value;
                        effective_num -= 1;
                    }
                    Err(e) => {
                        Timer::after_ticks(10).await;
                    }
                }
            }
        }
    }
    let avg = adc_valute_sum / 20;
    
    let temp = if avg < 400 {
        2
    } else {
        3
    };
    
    if avg > 1000 {
        return 0;
    }
    
    temp
}