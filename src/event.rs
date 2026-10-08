use alloc::boxed::Box;

use heapless::Vec;

use core::future::Future;
use core::pin::Pin;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::InputPin;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcPin};
use esp_println::println;
use esp_hal::gpio::{Input, Pull};
use crate::sleep::refresh_active_time;

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
    callback:Box< dyn FnMut(EventInfo) -> Pin<Box< dyn Future<Output = ()>  + 'static>>   + Send + Sync + 'static>,
    event_type:EventType,
    ptr:Option<usize>,
    fixed:bool,
}

static LISTENER:Mutex<CriticalSectionRawMutex,Vec<Listener,20>>  = Mutex::new(Vec::new()) ;

/// 按键事件队列：容量 8。生产者=toggle_event（扫描/状态机），消费者=dispatch 任务。
static EVENT_QUEUE:Mutex<CriticalSectionRawMutex,Vec<EventType,8>>  = Mutex::new(Vec::new()) ;
static EVENT_READY:Signal<CriticalSectionRawMutex,()>  = Signal::new();
pub async fn on<F>(event_type: EventType, callback: F)
where F: FnMut(EventInfo) -> Pin<Box<dyn Future<Output=()>  + 'static>>  + Send + Sync + 'static,
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:None,fixed:false});
}
pub async fn on_target<F>(event_type: EventType,target_ptr:usize, callback: F)
    where F: FnMut(EventInfo) -> Pin<Box<dyn Future<Output=()>  + 'static>>  + Send + Sync + 'static
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:Some(target_ptr),fixed:false});
}
pub async fn on_fixed<F>(event_type: EventType,target_ptr:usize, callback: F)
    where F: FnMut(EventInfo) -> Pin<Box<dyn Future<Output=()> + 'static>>  + Send + Sync + 'static
{
    LISTENER.lock().await.push(Listener{callback:Box::new(callback),event_type,ptr:Some(target_ptr),fixed:true});
}

pub async fn clear(){
    // fixed 项跨页面切换保留（on_fixed 的语义）；其余随页面重绑定清除
    LISTENER.lock().await.retain(|l| l.fixed);
}


/// 事件入队（生产端）：扫描/状态机只管产出，派发由 dispatch 任务串行执行——
/// 长回调期间扫描不停，按键不丢。KeyLongIng 连发在队尾合并；队列满丢最旧保最新。
pub async fn toggle_event(event_type: EventType,_ms:u64){
    println!("event_type:{:?}",event_type);
    let mut q = EVENT_QUEUE.lock().await;
    if let EventType::KeyLongIng(k) = event_type {
        if let Some(EventType::KeyLongIng(k2)) = q.last() {
            if *k2 == k {
                q.pop(); // 队尾同键连发 → 原位替换，不堆积
            }
        }
    }
    if q.len() >= q.capacity() {
        q.remove(0); // 满：丢最旧保最新
    }
    q.push(event_type).ok();
    drop(q);
    EVENT_READY.signal(());
}

/// 事件派发任务：FIFO 串行派发到监听回调。
/// 注意：派发期间持有 LISTENER 锁且回调串行执行——回调内禁止调用
/// on/clear/on_fixed 重绑定事件（会死锁）；重绑定走页面 run 循环的约定不变。
#[embassy_executor::task]
pub async fn dispatch() {
    loop {
        EVENT_READY.wait().await;
        loop {
            let ev = {
                let mut q = EVENT_QUEUE.lock().await;
                if q.is_empty() { None } else { Some(q.remove(0)) }
            };
            match ev {
                Some(ev) => dispatch_event(ev).await,
                None => break,
            }
        }
    }
}

async fn dispatch_event(event_type: EventType) {
    let mut vec = LISTENER.lock().await;
    for listener in vec.iter_mut() {
        if listener.event_type == event_type{

            (listener.callback)(EventInfo{ptr:listener.ptr }).await;

        }
    }

}


#[embassy_executor::task]
#[cfg(feature = "board_v2")]
pub async fn run(key2: crate::board::KeyLadderGpio){
    // 三键共用 GPIO4，按下都会拉到数字低电平（实测键1≈20mV/键2≈282mV/
    // 键3≈519mV，均低于数字低阈值）→ 下降沿检测按键，ADC 平均值仅用于区分键号
    let mut key_all = Input::new(key2, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
    loop {
        key_all.wait_for_falling_edge().await;
        // 边沿后立即采样区分键号（快速点按可能在几十 ms 内松开，
        // 采样必须赶在松开前完成）；中位数抗过渡期毛刺
        let num = judge_adc_num().await;
        if num == 0 {
            Timer::after_millis(10).await;
            continue;
        }
        key_detection(&mut key_all, num).await;
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
        Timer::after_millis(10).await;
    }
}

/// 旧板（epd2in9/epd2in7）：数字键 GPIO9（键1）+ 三键梯 GPIO2（键2/3 走 ADC），
/// 两路下降沿 select（master 行为，接入统一的 mark_user_touch）。
#[embassy_executor::task]
#[cfg(feature = "board_v1")]
pub async fn run(
    key1: esp_hal::peripherals::GPIO9<'static>,
    key2: crate::board::KeyLadderGpio,
){
    let mut key1 = Input::new(key1, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
    let mut key2 = Input::new(key2, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
    loop {
        match embassy_futures::select::select(
            key1.wait_for_falling_edge(),
            key2.wait_for_falling_edge(),
        )
        .await
        {
            embassy_futures::select::Either::First(_) => {
                key_detection(&mut key1, 1).await;
            }
            embassy_futures::select::Either::Second(_) => {
                let num = judge_adc_num().await;
                if num == 0 {
                    Timer::after_millis(10).await;
                    continue;
                }
                key_detection(&mut key2, num).await;
            }
        }
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
        Timer::after_millis(10).await;
    }
}

// 按键 ADC 引脚类型随板型走（见 board::KeyLadderGpio）
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<AdcPin<crate::board::KeyLadderGpio, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>>> = Mutex::new(None);
pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>>> = Mutex::new(None);

pub static  ENABLE_DOUBLE:Mutex<CriticalSectionRawMutex,bool> = Mutex::new(false);
pub async fn enable_double_click(){
    *ENABLE_DOUBLE.lock().await = true;
}
pub async fn disable_double_click(){
    *ENABLE_DOUBLE.lock().await = false;
}
pub async fn key_detection<P:InputPin>(key: &mut P, key_num: usize)
{
    // KeyLongIng 节流：主循环 ~1ms 一轮，不限流会 ~1kHz 派发、纯耗电。
    const LONG_ING_INTERVAL_MS: u64 = 100;
    let begin_ms = Instant::now().as_millis();
    let mut last_long_ing_ms = begin_ms;
    let mut is_long = false;

    loop {
        let mut is_low_times = 0;
        for _i in 0..100 {
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
                    last_long_ing_ms = current;
                    toggle_event(EventType::KeyLongStart(key_num as u32), current).await;
                }else if current - last_long_ing_ms >= LONG_ING_INTERVAL_MS {
                    last_long_ing_ms = current;
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
                    if !*ENABLE_DOUBLE.lock().await {
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    if current - begin_ms > 400 {
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    let mut is_low_times = 0;
                    for _i in 0..100 {
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

/// v22 三键梯（GPIO4，内部 45k 上拉）实测分压（12bit）：
/// 键1 ≈ 44 / 键2 ≈ 409 / 键3 ≈ 645——按下都低于数字低阈值，故用边沿检测按键、
/// ADC 平均值仅区分键号。阈值取相邻实测值的中点；明显高于键3 → 视为抖动忽略。
async fn judge_adc_num() -> usize {
    let mut buf = [0u16; 12];
    for slot in buf.iter_mut() {
        loop {
            let v = {
                let mut pin = ADC_PIN.lock().await;
                let mut adc = ADC_PER.lock().await;
                if let (Some(pin), Some(adc)) = (pin.as_mut(), adc.as_mut()) {
                    adc.read_oneshot(pin).ok()
                } else {
                    None
                }
            };
            if let Some(v) = v {
                *slot = v;
                break;
            }
            Timer::after_millis(2).await;
        }
    }
    buf.sort_unstable();
    let mid = buf[6] as u32;
    println!("[adc_key] press median={}", mid);

    // 键号判定按板型分：梯上键位布局与阈值实测值不同
    #[cfg(feature = "board_v2")]
    {
        // v2（GPIO4 梯，键1/2/3 都在梯上）：<220→键1（实测~25）；220~520→键2（~409）；520~900→键3（~645）
        if mid < 220 {
            1
        } else if mid < 520 {
            2
        } else if mid < 900 {
            3
        } else {
            0
        }
    }
    #[cfg(feature = "board_v1")]
    {
        // v1（GPIO2 梯，只有键2/3 在梯上，键1 走 GPIO9 直连不进此函数）：
        // <200→键2；200~1000→键3；>1000 抖动忽略（master 阈值）
        if mid < 200 {
            2
        } else if mid > 1000 {
            0
        } else {
            3
        }
    }
}
