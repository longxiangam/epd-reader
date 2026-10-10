# 第 4 篇：Embassy 异步运行时与事件驱动架构

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章
>
> English: [en/04-embassy-async-and-event-driven.md](en/04-embassy-async-and-event-driven.md)

## 引言

本项目要同时做很多事：刷电子墨水屏、读按键、收发网络包、计时、采样电池。这些并发由 Embassy 组织——一个基于 Rust async/await 的异步运行时。显示渲染、事件检测、网络通信、时间同步各自是一个 Embassy 任务。

本文讲 Embassy 在单核 ESP32-C3 上怎么调度这些任务，以及按键事件怎么从 GPIO 电平变化一步步变成页面响应。

## 1. Embassy 是什么？

### 1.1 核心思想

Embassy 的全称是 "Embedded Async"，它用 Rust 的 `async/await` 语法实现了一个轻量级的任务调度器。核心思想很简单：

- 每个 `async fn` 是一个**任务**
- 任务通过 `.await` 主动让出 CPU（协作式调度）
- 没有抢占，没有上下文切换的开销，不需要互斥量保护临界区

### 1.2 启动 Embassy

在 ESP32-C3 上，Embassy 通过 `esp-rtos` 集成：

```rust
// src/main.rs:125-127
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

`esp_rtos::start` 配置了 Embassy 执行器使用的硬件定时器和软件中断。ESP32-C3 的 Timer Group 0 (TIMG0) 提供精确的时间基准，软件中断用于唤醒挂起的任务。

之后，入口宏 `#[esp_rtos::main]` 会创建 Embassy 执行器，并将 `main` 函数作为第一个任务运行：

```rust
// src/main.rs:92-93
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
```

`Spawner` 是任务的"启动器"——通过它来创建新的异步任务。

### 1.3 任务创建

 Embassy 的任务用 `#[embassy_executor::task]` 宏标注：

```rust
// src/display.rs:114-120
#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: Input<'static>,
    rst: Output<'static>,
    dc: Output<'static>,
)
```

在 `main` 中通过 `spawner.spawn` 启动：

```rust
// src/main.rs:280
spawner.spawn(display::render(spi_bus_epd, epd_busy_in, epd_rst_out, epd_dc_out).unwrap());
```

注意：任务参数必须拥有 `'static` 生命周期。这是因为任务一旦启动，它可能在任何时候运行，它的参数必须比整个程序的运行时间活得更长。在嵌入式 `#![no_std]` 环境中，`'static` 是唯一的"永久"生命周期。

### 1.4 本项目的 Embassy 任务全景

| 任务 | 文件 | 职责 |
|------|------|------|
| `display::render` | display.rs | EPD 渲染循环（等待 Channel，驱动 SPI） |
| `event::run` | event.rs | 按键扫描（板型双变体），消抖/状态机后只做事件入队 |
| `event::dispatch` | event.rs | 事件队列消费者，串行派发到监听回调 |
| `battery::test_bat_adc` | battery.rs | 电池 ADC 采样（60 秒周期） |
| `worldtime::ntp_worker` | worldtime.rs | NTP 同步 + 定时拉取天气/节假日 |
| `pages::main_task` | pages/mod.rs | 正常模式主界面事件循环 |
| `pages::reading_task` | pages/mod.rs | 阅读模式入口（重启分模式，独占堆做 TTF） |
| `aht20::sample_task` | aht20.rs | 板载温湿度采样（仅 board_v2，60 秒周期） |
| `connection_wifi` | wifi.rs | WiFi STA 连接状态管理 |
| `net_task` | wifi.rs | 网络协议栈 Runner |
| `do_stop` | wifi.rs | WiFi 自动关闭（30 秒无使用超时，3 秒轮询） |
| `ap_task` | wifi.rs | AP 模式网络协议栈 Runner |
| `connection_wifi_ap` | wifi.rs | AP 模式连接管理 |
| `dhcp_service` | wifi.rs | AP 模式 DHCP 服务 |
| `dns_service` | wifi.rs | AP 模式 DNS 劫持 |
| `web_service` | web_service.rs | Web 配置服务（设置页按需 spawn） |

其中 STA 三件套（`net_task` / `connection_wifi` / `do_stop`）在 `connect_wifi` 里一起 spawn，AP 四件套在 `start_wifi_ap` 里一起 spawn；`aht20::sample_task` 与 `web_service` 按板型/页面条件启动。

## 2. 协作式调度：如何工作

### 2.1 .await 的本质

在 Embassy 中，每个 `.await` 都是一个**让出点**。当任务执行到 `.await` 时，如果等待的条件不满足，任务会保存自己的状态（栈帧），将控制权交还给执行器。执行器会选择下一个就绪的任务运行。

```rust
// 等待 100 毫秒
Timer::after(Duration::from_millis(100)).await;  // 让出 CPU，100ms 后恢复

// 等待 Channel 消息
let msg = receiver.receive().await;  // 让出 CPU，有消息时恢复

// 等待 GPIO 下降沿
key_all.wait_for_falling_edge().await;  // 让出 CPU，电平变化时恢复
```

### 2.2 select：多事件竞争

`embassy_futures::select::select` 允许同时等待多个 Future，谁先完成就处理谁。以旧板（board_v1）的双路按键为例：

```rust
// src/event.rs:156-178（board_v1 变体）
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
```

两个按键源，谁被按下就处理谁。`select` 不会阻塞在第一个等待上——它同时监听两个事件。新板（board_v2）三键共用一个 GPIO 梯，单路 `wait_for_falling_edge` 即可，用不到 select（见 4.1）。

同样在渲染任务中：

```rust
// src/display.rs:144-146
let render_sign = receiver.receive();        // 等待渲染请求
let quickly_lut = quickly_lut.receive();  // 等待刷新模式切换
match select(render_sign, quickly_lut).await {
    // 处理先到的那个
}
```

### 2.3 协作式调度的特点

Embassy 是**协作式**调度：任务只在 `.await` 让出时才会切换，不会被中途抢占。由此带来两个特点：

- 两段不 `.await` 的代码天然是原子的，访问普通共享数据不需要加锁（访问 `static mut` 仍需 `CriticalSectionRawMutex`）。
- 任务不分配独立栈，共享一个栈，每个任务自身只占很少内存。

代价是：任务必须自觉 `.await`。如果某个任务跑死循环不让出，其它任务就永远得不到执行机会——所以每个任务循环里都得有 `.await`。渲染刷新期间逐 2ms 轮询 BUSY（见第 3 篇）、按键长按状态机逐 1ms 轮询电平，都是在等的时候让出 CPU 的例子。

## 3. 事件系统设计

### 3.1 事件类型

本项目定义了丰富的事件类型，覆盖所有按键交互模式：

```rust
// src/event.rs:17-26
pub enum EventType{
    KeyShort(u32),       // 短按，参数是按键编号
    KeyLongStart(u32),   // 长按开始
    KeyLongIng(u32),     // 长按持续中（节流为每 100ms 入队一次）
    KeyLongEnd(u32),     // 长按释放
    KeyDouble(u32),      // 双击
    WheelBack,           // 滚轮后退（预留）
    WheelFront,          // 滚轮前进（预留）
}
```

事件携带的信息：

```rust
// src/event.rs:28-31
pub struct EventInfo{
    pub ptr:Option<usize>,
}
```

### 3.2 发布-订阅机制

事件系统的核心是一个全局监听器列表：

```rust
// src/event.rs:37-44
struct Listener{
    callback:Box< dyn FnMut(EventInfo) -> Pin<Box< dyn Future<Output = ()>  + 'static>>   + Send + Sync + 'static>,
    event_type:EventType,
    ptr:Option<usize>,
    fixed:bool,
}

static LISTENER:Mutex<CriticalSectionRawMutex,Vec<Listener,20>>  = Mutex::new(Vec::new()) ;
```

这个类型签名看起来很复杂，但拆解后逻辑很清晰：
- **callback**：一个闭包，接收 `EventInfo`，返回一个 `Future`
- **event_type**：监听的事件类型
- **ptr**：可选的对象指针，用于事件与对象的绑定
- **fixed**：标记是否在 `clear()` 时保留

### 3.3 三种注册方式

```rust
// 一次性监听（clear() 时会被清除）
pub async fn on<F>(event_type: EventType, callback: F)

// 绑定到特定对象的监听
pub async fn on_target<F>(event_type: EventType, target_ptr: usize, callback: F)

// 常驻监听（clear() 时不会被清除）
pub async fn on_fixed<F>(event_type: EventType, target_ptr: usize, callback: F)
```

为什么需要三种？这与页面系统的生命周期有关：

1. **`on()`**：页面进入时注册，页面退出时通过 `clear()` 统一清除。大多数事件监听用这个。

2. **`on_target()`**：同上，但绑定了特定对象的指针。在事件回调中可以通过 `ptr` 找到对应的对象。

3. **`on_fixed()`**：常驻监听，不受 `clear()` 影响。用于全局性的功能（如睡眠检测）。

### 3.4 事件队列与串行派发

事件的生产和消费是**两个任务**：扫描/状态机只管产出（入队），派发由独立的 `dispatch` 任务串行执行。

生产端 `toggle_event` 只入队，永不同步执行回调：

```rust
// src/event.rs:46-89
/// 按键事件队列：容量 8。生产者=toggle_event（扫描/状态机），消费者=dispatch 任务。
static EVENT_QUEUE:Mutex<CriticalSectionRawMutex,Vec<EventType,8>>  = Mutex::new(Vec::new()) ;
static EVENT_READY:Signal<CriticalSectionRawMutex,()>  = Signal::new();

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
```

消费端 `dispatch` 任务等 Signal、FIFO 出队、逐个派发：

```rust
// src/event.rs:91-121
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
```

三条防丢失/防堆积规则值得注意：

- **队列满（容量 8）丢最旧保最新**——用户快速连按多下时，最新意图优先；
- **`KeyLongIng` 队尾同键合并**——长按每 100ms 才入队一次，且若队尾已是同键的 `KeyLongIng` 就原位替换，长按再久也只占一个槽位；
- **派发串行**——同一时间只有一个回调在执行，回调之间不会交叠。

有一个必须遵守的约束：`dispatch_event` 在**持有 LISTENER 锁**的状态下执行回调，因此**回调内禁止调用 `on` / `clear` / `on_fixed` 重新绑定事件**——那是再拿同一把锁，会死锁。重绑定仍走页面 `run` 循环的约定：页面切换时由 `bind_event()` 在派发任务之外执行（先 `clear()` 再注册）。

收益：某个页面回调里做长耗时操作（比如翻页时读 SD + TTF 栅格化几百毫秒）期间，`event::run` 扫描任务照常运行、新按键照常入队，不再像"同步遍历派发"的旧设计那样整段时间丢按键。

### 3.5 页面中的事件绑定

以主页面为例，展示事件绑定的实际使用：

```rust
// src/pages/main_page.rs:188-229（节选）
async fn bind_event(&mut self){
    event::clear().await;  // 清除上一个页面的事件监听（fixed 项保留）

    // Key2 短按 → 下移光标
    event::on(EventType::KeyShort(2),  move |_info|  {
        return Box::pin(async {
            Self::get_mut().await.unwrap().increase();
        });
    }).await;

    // KeyLongEnd(1)/(2)：主页面长按结束无动作，占位注册
    event::on(EventType::KeyLongEnd(1),  |_info|  { ... }).await;

    // Key1 短按 → 上移光标
    event::on(EventType::KeyShort(1),  |_info|  {
        return Box::pin( async {
            Self::get_mut().await.unwrap().decrease();
        });
    }).await;

    // Key3 短按 → 进入子页面（记录页号 + 写睡眠页，唤醒直接回到该应用）
    event::on(EventType::KeyShort(3),  |_info|  {
        return Box::pin( async {
            let mut_ref = Self::get_mut().await.unwrap();
            mut_ref.current_page = Some( mut_ref.choose_index);
            unsafe { *core::ptr::addr_of_mut!(PAGE_INDEX) = mut_ref.choose_index as i32; }
            crate::storage::write_sleep_page(mut_ref.choose_index as i32);
        });
    }).await;
}
```

这里有一个特殊的模式：闭包内部通过 `Self::get_mut().await` 获取页面的可变引用。这是因为 Rust 闭包不能直接捕获 `&mut self`（生命周期问题），所以通过全局静态变量间接访问：

```rust
// src/pages/main_page.rs:132-137
pub async fn get_mut() -> Option<&'static mut MainPage> {
    unsafe {
        let ptr: *mut MainPage =  MAIN_PAGE.lock().await.as_mut().unwrap()  as *mut MainPage;
        Some(&mut *ptr)
    }
}
```

这是一个 `unsafe` 操作，但在本项目的协作式调度下是安全的——因为同一时间只有 dispatch 任务在处理事件，不会出现并发访问。

## 4. 按键检测：从电平到事件

### 4.1 板型双变体：三键怎么接到一个 GPIO 上

硬件上三颗按键通过 ADC 分压梯共用一个 GPIO（`board::KeyLadderGpio`，随板型而定：v1=GPIO2，v2=GPIO4）：每颗按键串联不同阻值的电阻到地，按下时读到的电压不同：

```
GPIO(梯) ──┬──[R1]── Key1 ── GND      ← v2：三键全在梯上
           ├──[R2]── Key2 ── GND
           └──[R3]── Key3 ── GND
```

- **v2 新板（board_v2）**：三键全在 GPIO4 梯上（内部 45k 上拉）。实测分压键1≈44 / 键2≈409 / 键3≈645（12bit），都低于数字低阈值——所以按下都会拉低电平，用**下降沿触发**检测"有键按下"，再用 ADC **中位数**区分是哪颗键。
- **v1 旧板（board_v1）**：键1 走 GPIO9 数字直连，键2/3 在 GPIO2 梯上，两路下降沿 `select`（见 2.2）；梯上部分仍用 ADC 区分键号。

两处循环在每轮按键处理完后都会调用 `refresh_active_time()`（刷新活跃时刻）和 `sleep::mark_user_touch()`（标记"用户碰过键"，退出定时唤醒的"刷完即睡"模式）。

### 4.2 按键状态机

`key_detection` 函数实现了一个状态机，区分短按、长按、双击：

```rust
// src/event.rs:192-259（简化流程）
pub async fn key_detection<P:InputPin>(key: &mut P, key_num: usize)
{
    // KeyLongIng 节流：主循环 ~1ms 一轮，不限流会 ~1kHz 派发、纯耗电。
    const LONG_ING_INTERVAL_MS: u64 = 100;
    let begin_ms = Instant::now().as_millis();
    let mut last_long_ing_ms = begin_ms;
    let mut is_long = false;

    loop {
        // 采样 100 次判断电平
        let mut is_low_times = 0;
        for _i in 0..100 {
            if key.is_low().unwrap() { is_low_times += 1; }
        }

        if is_low_times > 80 {
            // 按键处于按下状态
            let current = Instant::now().as_millis();
            if current - begin_ms > 500 {
                if !is_long {
                    is_long = true;
                    last_long_ing_ms = current;
                    toggle_event(EventType::KeyLongStart(key_num as u32), current).await;
                } else if current - last_long_ing_ms >= LONG_ING_INTERVAL_MS {
                    last_long_ing_ms = current;  // 每 100ms 入队一次，避免 ~1kHz 空转耗电
                    toggle_event(EventType::KeyLongIng(key_num as u32), current).await;
                }
            }
        } else if is_low_times < 2 {
            // 按键释放
            let current = Instant::now().as_millis();
            if is_long {
                toggle_event(EventType::KeyLongEnd(key_num as u32), current).await;
                return;
            } else {
                // 短按：等待看是否双击
                loop {
                    let current = Instant::now().as_millis();
                    if !*ENABLE_DOUBLE.lock().await {
                        // 全局关闭双击：立即按短按派发
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    if current - begin_ms > 400 {
                        // 400ms 内没有第二次按下：短按
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    // 再次按下 → 双击
                    if /* 采样 100 次多数为低 */ {
                        toggle_event(EventType::KeyDouble(key_num as u32), current).await;
                        return;
                    }
                }
            }
        }
        Timer::after(Duration::from_millis(1)).await;
    }
}
```

状态机可以图示为：

```
                   按下
  [空闲] ─────────────► [按下中]
     ▲                    │
     │                    ├── < 500ms 释放 ──► [短按判定]
     │                    │                      ├── 400ms 内再按 ──► KeyDouble
     │                    │                      └── 超时 ──► KeyShort
     │                    │
     │                    └── > 500ms ──► [长按中]
     │                                      │
     │                                      ├── KeyLongStart（首次）
     │                                      ├── KeyLongIng（持续，100ms 节流入队）
     │                                      └── 释放 ──► KeyLongEnd
     │                                                      │
     └──────────────────────────────────────────────────────┘
```

采样 100 次取多数的消抖策略（80/100 为阈值），比传统的延时消抖更可靠。每次循环通过 `Timer::after(1ms).await` 让出 CPU，不会阻塞其他任务。

### 4.3 ADC 按键识别

`judge_adc_num` 采 12 个样本做**中位数**判定——中位数对过渡期毛刺和偶发坏点天然免疫，比平均值稳：

```rust
// src/event.rs:265-315（简化，板型阈值分支）
async fn judge_adc_num() -> usize {
    let mut buf = [0u16; 12];
    for slot in buf.iter_mut() {
        loop {
            let v = { /* ADC_PIN/ADC_PER 持锁，adc.read_oneshot(pin).ok() */ };
            if let Some(v) = v { *slot = v; break; }
            Timer::after_millis(2).await;   // 冷启动/通道切换常返 Err：等 2ms 重试
        }
    }
    buf.sort_unstable();
    let mid = buf[6] as u32;   // 中位数
    println!("[adc_key] press median={}", mid);

    // 键号判定按板型分：梯上键位布局与阈值实测值不同
    #[cfg(feature = "board_v2")]
    {
        // v2（GPIO4 梯，键1/2/3 都在梯上，实测 ~44/409/645）：
        if mid < 220 { 1 }        // 键1（实测 ~44）
        else if mid < 520 { 2 }   // 键2（实测 ~409）
        else if mid < 900 { 3 }   // 键3（实测 ~645）
        else { 0 }                // 明显高于键3 → 视为抖动忽略
    }
    #[cfg(feature = "board_v1")]
    {
        // v1（GPIO2 梯，只有键2/3 在梯上，键1 走 GPIO9 直连不进此函数）：
        if mid < 200 { 2 }        // 键2（低电压）
        else if mid > 1000 { 0 }  // 抖动忽略（接近上拉电平）
        else { 3 }                // 键3（较高电压）
    }
}
```

边沿触发后要立即采样区分键号——快速点按可能在几十毫秒内就松开，采样必须赶在松开前完成，这也是边沿后第一件事就是 `judge_adc_num` 的原因。返回 0（抖动/无键）时本次丢弃，10ms 后回到扫描循环。

电池采样（`battery.rs`）采用同一套"出错重试、超限放弃"策略（上限 50 次，保留上次电量），保证 ADC 偶发错误不会让按键或电量检测整体失效。

## 5. Embassy 同步原语

本项目使用了 Embassy 提供的三种同步原语，各有适用场景：

### 5.1 Channel：异步消息传递

用于生产者-消费者模式：

```rust
pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex, RenderInfo, 64> = Channel::new();
```

- 容量 64，足够缓冲多个渲染请求
- 发送端不会阻塞（除非满了）
- 接收端通过 `.receive().await` 等待新消息

事件队列是同款思路的"有界 + 丢旧"变体：`Mutex<Vec<EventType, 8>>` + `Signal` 组合，容量更小但语义更强（新事件优先）。

### 5.2 Signal：一次性信号

用于简单的通知场景：

```rust
pub static STOP_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
```

Signal 只保留最新的值，适合"开关"式的通知：启动 WiFi、停止 WiFi、重新连接等。事件队列的 `EVENT_READY` 也是 Signal——只表示"有新事件"，具体是什么由队列自己说。

### 5.3 Mutex：异步互斥锁

用于保护共享数据：

```rust
pub static WIFI_INFO:Mutex<CriticalSectionRawMutex,Option<WifiStorage>>  =  Mutex::new(None);
```

`lock().await` 在锁可用时立即返回，否则让出 CPU 等待。由于是协作式调度，不存在死锁的风险——只要每个任务在持有锁时不忘记 `.await`（或者不在持锁期间做长时间操作）。唯一的例外是 3.4 提到的：dispatch 持 LISTENER 锁执行回调，回调里再去拿同一把锁就会死锁，这是全项目唯一需要人肉遵守的加锁纪律。

## 6. 完整的事件流

从按键到屏幕更新的完整数据流：

```
物理按键按下
    │
GPIO 电平变化（下降沿）
    │
event::run() — wait_for_falling_edge().await
    │                 （v2：judge_adc_num 中位数定键号；
    │                   v1：GPIO9 直连键1 或 GPIO2 梯 select）
    │
key_detection() — 消抖/状态机 → KeyShort/KeyLong*/KeyDouble
    │
toggle_event() — 事件入队 EVENT_QUEUE + EVENT_READY.signal()
    │
event::dispatch 任务 — Signal 唤醒 → FIFO 出队
    │
dispatch_event() — 遍历 LISTENER（持锁），匹配事件类型
    │
Listener callback — 修改页面状态（如 choose_index += 1）
    │
页面 render() — 绘制到帧缓冲区
    │
RENDER_CHANNEL.send() — 发送渲染请求
    │
display::render() — 接收请求，驱动 SPI 刷新屏幕
    │
EPD 显示新画面
```

扫描（生产）与派发（消费）被队列隔开：回调再慢，扫描也不停、按键不丢。

这条链路贯穿了本项目的三个核心层次：**事件层 → 页面层 → 显示层**，每一层都通过 Embassy 的异步机制解耦。

## 小结

| 技术点 | 要点 |
|--------|------|
| Embassy 执行器 | `esp_rtos::start` + `#[esp_rtos::main]` |
| 任务创建 | `#[embassy_executor::task]` + `spawner.spawn()` |
| 协作式调度 | `.await` 让出 CPU，无抢占，无临界区 |
| select | 同时等待多个 Future，谁先就绪处理谁（v1 双路按键 / 渲染双 Channel） |
| 事件系统 | 队列（容量 8，丢旧保新、KeyLongIng 合并）+ dispatch 任务串行派发 + 三种注册模式（on/on_target/on_fixed） |
| 按键检测 | 边沿触发 + 100 次采样消抖 + 状态机（100ms 长按节流）+ ADC 中位数分键（板型双阈值组） |
| 同步原语 | Channel（消息）、Signal（通知）、Mutex（共享数据） |

Embassy 的设计哲学是：**用 Rust 的类型系统和 async/await 语法，在编译期保证并发安全，在运行时实现高效的协作式调度**。这让嵌入式开发既安全又高效。

在下一篇文章中，我们将进入 WiFi 网络的世界，看看 ESP32-C3 如何在 400KB SRAM 的限制下跑起完整的 TCP/TLS 协议栈。

---

> 上一篇：[第 3 篇：电子墨水屏驱动与渲染架构](03-电子墨水屏驱动与渲染架构.md) · 下一篇：[第 5 篇：WiFi 网络与 HTTP 通信](05-WiFi网络与HTTP通信.md)
