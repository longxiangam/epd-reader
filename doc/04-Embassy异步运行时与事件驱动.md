# 第 4 篇：Embassy 异步运行时与事件驱动架构

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

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
// src/main.rs:86-89
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

`esp_rtos::start` 配置了 Embassy 执行器使用的硬件定时器和软件中断。ESP32-C3 的 Timer Group 0 (TIMG0) 提供精确的时间基准，软件中断用于唤醒挂起的任务。

之后，入口宏 `#[esp_rtos::main]` 会创建 Embassy 执行器，并将 `main` 函数作为第一个任务运行：

```rust
// src/main.rs:78-79
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
```

`Spawner` 是任务的"启动器"——通过它来创建新的异步任务。

### 1.3 任务创建

 Embassy 的任务用 `#[embassy_executor::task]` 宏标注：

```rust
// src/display.rs:80-86
#[embassy_executor::task]
pub async fn render(
    mut spi_device: &'static mut ActualSpi<'static>,
    busy: esp_hal::peripherals::GPIO6<'static>,
    rst: esp_hal::peripherals::GPIO7<'static>,
    dc: esp_hal::peripherals::GPIO20<'static>,
) {
    // ...
}
```

在 `main` 中通过 `spawner.spawn` 启动：

```rust
// src/main.rs:173
spawner.spawn(display::render(spi_bus_epd, epd_busy, epd_rst, epd_dc)).unwrap();
```

注意：任务参数必须拥有 `'static` 生命周期。这是因为任务一旦启动，它可能在任何时候运行，它的参数必须比整个程序的运行时间活得更长。在嵌入式 `#![no_std]` 环境中，`'static` 是唯一的"永久"生命周期。

### 1.4 本项目的 Embassy 任务全景

| 任务 | 文件 | 职责 |
|------|------|------|
| `display::render` | display.rs | EPD 渲染循环（等待 Channel，驱动 SPI） |
| `event::run` | event.rs | 按键检测与事件分发 |
| `battery::test_bat_adc` | battery.rs | 电池 ADC 采样（60 秒周期） |
| `worldtime::ntp_worker` | worldtime.rs | NTP 同步 + 定时拉取天气/节假日 |
| `pages::main_task` | pages/mod.rs | 主界面事件循环 |
| `connection_wifi` | wifi.rs | WiFi STA 连接状态管理 |
| `net_task` | wifi.rs | 网络协议栈 Runner |
| `do_stop` | wifi.rs | WiFi 自动关闭（30 秒超时） |
| `dhcp_service` | wifi.rs | AP 模式 DHCP 服务 |
| `dns_service` | wifi.rs | AP 模式 DNS 劫持 |

## 2. 协作式调度：如何工作

### 2.1 .await 的本质

在 Embassy 中，每个 `.await` 都是一个**让出点**。当任务执行到 `.await` 时，如果等待的条件不满足，任务会保存自己的状态（栈帧），将控制权交还给执行器。执行器会选择下一个就绪的任务运行。

```rust
// 等待 100 毫秒
Timer::after(Duration::from_millis(100)).await;  // 让出 CPU，100ms 后恢复

// 等待 Channel 消息
let msg = receiver.receive().await;  // 让出 CPU，有消息时恢复

// 等待 GPIO 下降沿
key1.wait_for_falling_edge().await;  // 让出 CPU，电平变化时恢复
```

### 2.2 select：多事件竞争

`embassy_futures::select::select` 允许同时等待多个 Future，谁先完成就处理谁：

```rust
// src/event.rs:104-116
loop {
    let key1_edge = key1.wait_for_falling_edge();
    let key2_edge = key2.wait_for_falling_edge();
    match select(key1_edge, key2_edge).await {
        First(_) => {
            key_detection::<_, 1>(&mut key1).await;
        }
        Second(_) => {
            key_detection::<_, 2>(&mut key2).await;
        }
    }
    refresh_active_time().await;
    Timer::after(Duration::from_millis(10)).await;
}
```

两个按键，谁被按下就处理谁。`select` 不会阻塞在第一个等待上——它同时监听两个事件。

同样在渲染任务中：

```rust
// src/display.rs:113-115
let render_sign = receiver.receive();        // 等待渲染请求
let quickly_lut = quickly_lut_receiver.receive();  // 等待刷新模式切换
match select(render_sign, quickly_lut).await {
    // 处理先到的那个
}
```

### 2.3 协作式调度的特点

Embassy 是**协作式**调度：任务只在 `.await` 让出时才会切换，不会被中途抢占。由此带来两个特点：

- 两段不 `.await` 的代码天然是原子的，访问普通共享数据不需要加锁（访问 `static mut` 仍需 `CriticalSectionRawMutex`）。
- 任务不分配独立栈，共享一个栈，每个任务自身只占很少内存。

代价是：任务必须自觉 `.await`。如果某个任务跑死循环不让出，其它任务就永远得不到执行机会——所以每个任务循环里都得有 `.await`。

## 3. 事件系统设计

### 3.1 事件类型

本项目定义了丰富的事件类型，覆盖所有按键交互模式：

```rust
// src/event.rs:18-27
pub enum EventType {
    KeyShort(u32),       // 短按，参数是按键编号
    KeyLongStart(u32),   // 长按开始
    KeyLongIng(u32),     // 长按持续中（节流为每 100ms 触发一次）
    KeyLongEnd(u32),     // 长按释放
    KeyDouble(u32),      // 双击
    WheelBack,           // 滚轮后退（预留）
    WheelFront,          // 滚轮前进（预留）
}
```

事件携带的信息：

```rust
// src/event.rs:30-32
pub struct EventInfo {
    pub ptr: Option<usize>,  // 注册时绑定的对象指针
}
```

### 3.2 发布-订阅机制

事件系统的核心是一个全局监听器列表：

```rust
// src/event.rs:38-45
struct Listener {
    callback: Box<dyn FnMut(EventInfo) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + Sync + 'static>,
    event_type: EventType,
    ptr: Option<usize>,    // 绑定的对象指针
    fixed: bool,           // 是否常驻
}

static LISTENER: Mutex<CriticalSectionRawMutex, Vec<Listener, 20>> = Mutex::new(Vec::new());
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

### 3.4 事件分发

当事件触发时，遍历所有监听器，匹配事件类型并调用回调：

```rust
// src/event.rs:83-94
pub async fn toggle_event(event_type: EventType, _ms: u64) {
    let mut vec = LISTENER.lock().await;
    for listener in vec.iter_mut() {
        if listener.event_type == event_type {
            (listener.callback)(EventInfo { ptr: listener.ptr }).await;
        }
    }
}
```

注意：事件分发是同步的——所有匹配的回调会依次执行完毕后才返回。这在协作式调度下是安全的，因为没有抢占。

### 3.5 页面中的事件绑定

以主页面为例，展示事件绑定的实际使用：

```rust
// src/pages/main_page.rs:126-168
async fn bind_event(&mut self) {
    event::clear().await;  // 清除上一个页面的事件监听

    // Key2 短按 → 下移光标
    event::on(EventType::KeyShort(2), move |_info| {
        Box::pin(async {
            Self::get_mut().await.unwrap().increase();
        })
    }).await;

    // Key1 短按 → 上移光标
    event::on(EventType::KeyShort(1), |_info| {
        Box::pin(async {
            Self::get_mut().await.unwrap().decrease();
        })
    }).await;

    // Key3 短按 → 进入子页面
    event::on(EventType::KeyShort(3), |_info| {
        Box::pin(async {
            let mut_ref = Self::get_mut().await.unwrap();
            mut_ref.current_page = Some(mut_ref.choose_index);
        })
    }).await;
}
```

这里有一个特殊的模式：闭包内部通过 `Self::get_mut().await` 获取页面的可变引用。这是因为 Rust 闭包不能直接捕获 `&mut self`（生命周期问题），所以通过全局静态变量间接访问：

```rust
// src/pages/main_page.rs:77-82
pub async fn get_mut() -> Option<&'static mut MainPage> {
    unsafe {
        let ptr: *mut MainPage = MAIN_PAGE.lock().await.as_mut().unwrap() as *mut MainPage;
        Some(&mut *ptr)
    }
}
```

这是一个 `unsafe` 操作，但在本项目的协作式调度下是安全的——因为同一时间只有一个任务在处理事件，不会出现并发访问。

## 4. 按键检测：从电平到事件

### 4.1 多按键检测

本项目的三个物理按键中，Key2 和 Key3 通过 ADC 分压共用 GPIO2。硬件上，两个按键串联不同阻值的电阻到地：

```
GPIO2 ──┬──[R1]── Key2 ── GND
        └──[R2]── Key3 ── GND
```

按下 Key2 时 ADC 读到低电压，按下 Key3 时读到较高电压，都不按时读到最高电压。

### 4.2 按键状态机

`key_detection` 函数实现了一个状态机，区分短按、长按、双击：

```rust
// src/event.rs:129-200（简化流程）
async fn key_detection<P, const NUM: usize>(key: &mut P) {
    const LONG_ING_INTERVAL_MS: u64 = 100;          // KeyLongIng 节流间隔
    let begin_ms = Instant::now().as_millis();
    let mut last_long_ing_ms = begin_ms;
    let mut is_long = false;
    let mut key_num = NUM;

    // Key2 需要通过 ADC 判断是 Key2 还是 Key3
    if NUM == 2 {
        key_num = judge_adc_num().await;
        if key_num == 0 { return; }  // ADC 值太高，不是按键
    }

    loop {
        // 采样 100 次判断电平
        let is_low_times = /* 采样 */;

        if is_low_times > 80 {
            // 按键处于按下状态
            let current = Instant::now().as_millis();
            if current - begin_ms > 500 {
                if !is_long {
                    is_long = true;
                    last_long_ing_ms = current;
                    toggle_event(EventType::KeyLongStart(key_num)).await;
                } else if current - last_long_ing_ms >= LONG_ING_INTERVAL_MS {
                    last_long_ing_ms = current;  // 每 100ms 才派发一次，避免 ~1kHz 空转耗电
                    toggle_event(EventType::KeyLongIng(key_num)).await;
                }
            }
        } else if is_low_times < 2 {
            // 按键释放
            if is_long {
                toggle_event(EventType::KeyLongEnd(key_num)).await;
                return;
            } else {
                // 短按：等待看是否双击
                if enable_double_click {
                    // 等待 400ms，如果再次按下则触发双击
                    if /* 再次按下 */ {
                        toggle_event(EventType::KeyDouble(key_num)).await;
                    } else {
                        toggle_event(EventType::KeyShort(key_num)).await;
                    }
                } else {
                    toggle_event(EventType::KeyShort(key_num)).await;
                }
                return;
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
     │                                      ├── KeyLongIng（持续）
     │                                      └── 释放 ──► KeyLongEnd
     │                                                      │
     └──────────────────────────────────────────────────────┘
```

采样 100 次取多数的消抖策略（80/100 为阈值），比传统的延时消抖更可靠。每次循环通过 `Timer::after(1ms).await` 让出 CPU，不会阻塞其他任务。

### 4.3 ADC 按键识别

ESP32-C3 的 `read_oneshot` 在冷启动 / 通道切换时**经常返回 `Err`**，必须容忍——只采到 `Ok` 的样本计入平均，但仅在总尝试次数超限时才放弃，避免卡死事件任务：

```rust
// src/event.rs（简化）
async fn judge_adc_num() -> usize {
    const SAMPLES: usize = 20;
    const MAX_TRIES: usize = 200;
    const AVG_KEY2: u32 = 200;   // < 200 → Key2
    const AVG_NONE: u32 = 1000;  // > 1000 → 无按键（上拉到 VCC）

    let mut need = SAMPLES;
    let mut sum: u32 = 0;
    let mut tries = 0usize;
    while need > 0 {
        tries += 1;
        if tries > MAX_TRIES { return 0; }   // 持续 Err 超限：放弃，避免卡死
        if let Some(pin) = ADC_PIN.lock().await.as_mut() {
            if let Some(adc) = ADC_PER.lock().await.as_mut() {
                match adc.read_oneshot(pin) {
                    Ok(v)  => { sum += v as u32; need -= 1; }
                    Err(_) => { Timer::after_millis(2).await; }  // 偶发 Err：等 2ms 重试
                }
            }
        }
    }
    let avg = sum / SAMPLES as u32;
    if avg > AVG_NONE { 0 } else if avg < AVG_KEY2 { 2 } else { 3 }
}
```

20 次有效采样取平均值，阈值 200 区分 Key2（低电压）和 Key3（较高电压）；超过 1000 视为无按键返回 0。电池采样（`battery.rs`）采用同样的"出错重试、超限放弃"策略（上限 50 次），保证 ADC 偶发错误不会让按键或电量检测整体失效。

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

### 5.2 Signal：一次性信号

用于简单的通知场景：

```rust
pub static STOP_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
```

Signal 只保留最新的值，适合"开关"式的通知：启动 WiFi、停止 WiFi、重新连接等。

### 5.3 Mutex：异步互斥锁

用于保护共享数据：

```rust
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);
```

`lock().await` 在锁可用时立即返回，否则让出 CPU 等待。由于是协作式调度，不存在死锁的风险——只要每个任务在持有锁时不忘记 `.await`（或者不在持锁期间做长时间操作）。

## 6. 完整的事件流

从按键到屏幕更新的完整数据流：

```
物理按键按下
    │
GPIO 电平变化
    │
event::run() — wait_for_falling_edge().await
    │
key_detection() — ADC 采样 → 判断按键编号
    │
toggle_event() — 遍历 LISTENER，匹配事件类型
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

这条链路贯穿了本项目的三个核心层次：**事件层 → 页面层 → 显示层**，每一层都通过 Embassy 的异步机制解耦。

## 小结

| 技术点 | 要点 |
|--------|------|
| Embassy 执行器 | `esp_rtos::start` + `#[esp_rtos::main]` |
| 任务创建 | `#[embassy_executor::task]` + `spawner.spawn()` |
| 协作式调度 | `.await` 让出 CPU，无抢占，无临界区 |
| select | 同时等待多个 Future，谁先就绪处理谁 |
| 事件系统 | 发布-订阅 + 三种注册模式（on/on_target/on_fixed） |
| 按键检测 | 100 次采样消抖 + 状态机 + ADC 多按键 |
| 同步原语 | Channel（消息）、Signal（通知）、Mutex（共享数据） |

Embassy 的设计哲学是：**用 Rust 的类型系统和 async/await 语法，在编译期保证并发安全，在运行时实现高效的协作式调度**。这让嵌入式开发既安全又高效。

在下一篇文章中，我们将进入 WiFi 网络的世界，看看 ESP32-C3 如何在 400KB SRAM 的限制下跑起完整的 TCP/TLS 协议栈。

---

> 上一篇：[第 3 篇：电子墨水屏驱动与渲染架构](03-电子墨水屏驱动与渲染架构.md) · 下一篇：[第 5 篇：WiFi 网络与 HTTP 通信](05-WiFi网络与HTTP通信.md)
