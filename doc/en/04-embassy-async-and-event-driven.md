# Part 4: Embassy Async Runtime and Event-Driven Architecture

> Building an ESP32-C3 E-Paper Reader in Rust · Article Series
>
> 中文版: [../04-Embassy异步运行时与事件驱动.md](../04-Embassy异步运行时与事件驱动.md)

## Introduction

This project has to do many things at once: refresh an e-paper panel, read keys, send and receive network packets, keep time, sample the battery. All of this concurrency is organized by Embassy — an async runtime built on Rust's async/await. Display rendering, event detection, network communication and time synchronization are each an Embassy task.

This article covers how Embassy schedules these tasks on a single-core ESP32-C3, and how a key press travels step by step from a GPIO level change to a page response.

## 1. What Is Embassy?

### 1.1 The Core Idea

Embassy stands for "Embedded Async". It implements a lightweight task scheduler using Rust's `async/await` syntax. The core idea is simple:

- Every `async fn` is a **task**
- Tasks voluntarily yield the CPU at `.await` points (cooperative scheduling)
- No preemption, no context-switch overhead, no mutexes needed to protect critical sections

### 1.2 Starting Embassy

On the ESP32-C3, Embassy is integrated through `esp-rtos`:

```rust
// src/main.rs:125-127
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

`esp_rtos::start` configures the hardware timer and software interrupts used by the Embassy executor. The ESP32-C3's Timer Group 0 (TIMG0) provides an accurate time base; the software interrupt wakes parked tasks.

Afterwards, the entry macro `#[esp_rtos::main]` creates the Embassy executor and runs the `main` function as the first task:

```rust
// src/main.rs:92-93
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
```

`Spawner` is the task launcher — new async tasks are created through it.

### 1.3 Creating Tasks

 Embassy tasks are annotated with the `#[embassy_executor::task]` macro:

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

And started in `main` via `spawner.spawn`:

```rust
// src/main.rs:280
spawner.spawn(display::render(spi_bus_epd, epd_busy_in, epd_rst_out, epd_dc_out).unwrap());
```

Note: task arguments must have a `'static` lifetime. Once a task is started it may run at any time, so its arguments must outlive the entire program. In an embedded `#![no_std]` environment, `'static` is the only "forever" lifetime.

### 1.4 The Full Task Landscape of This Project

| Task | File | Responsibility |
|------|------|----------------|
| `display::render` | display.rs | EPD rendering loop (waits on the Channel, drives SPI) |
| `event::run` | event.rs | Key scanning (board-specific variant); after debounce/state machine, only enqueues events |
| `event::dispatch` | event.rs | Event queue consumer; serially dispatches to listener callbacks |
| `battery::test_bat_adc` | battery.rs | Battery ADC sampling (60-second period) |
| `worldtime::ntp_worker` | worldtime.rs | NTP sync + periodic weather/holiday fetches |
| `pages::main_task` | pages/mod.rs | Main-menu event loop in normal mode |
| `pages::reading_task` | pages/mod.rs | Reading-mode entry (reboot-based mode split, heap dedicated to TTF) |
| `aht20::sample_task` | aht20.rs | On-board temperature/humidity sampling (board_v2 only, 60-second period) |
| `connection_wifi` | wifi.rs | WiFi STA connection state management |
| `net_task` | wifi.rs | Network stack Runner |
| `do_stop` | wifi.rs | Automatic WiFi shutdown (30s idle timeout, polled every 3s) |
| `ap_task` | wifi.rs | AP-mode network stack Runner |
| `connection_wifi_ap` | wifi.rs | AP-mode connection management |
| `dhcp_service` | wifi.rs | AP-mode DHCP service |
| `dns_service` | wifi.rs | AP-mode DNS hijacking |
| `web_service` | web_service.rs | Web configuration service (spawned on demand by the settings page) |

The STA trio (`net_task` / `connection_wifi` / `do_stop`) is spawned together inside `connect_wifi`, the AP quartet inside `start_wifi_ap`; `aht20::sample_task` and `web_service` start conditionally by board/page.

## 2. Cooperative Scheduling: How It Works

### 2.1 What .await Really Is

In Embassy, every `.await` is a **yield point**. When a task reaches `.await` and the awaited condition is not satisfied, it saves its state (stack frame) and hands control back to the executor, which picks the next ready task.

```rust
// Wait 100 milliseconds
Timer::after(Duration::from_millis(100)).await;  // yield the CPU, resume after 100ms

// Wait for a Channel message
let msg = receiver.receive().await;  // yield, resume when a message arrives

// Wait for a GPIO falling edge
key_all.wait_for_falling_edge().await;  // yield, resume when the level changes
```

### 2.2 select: Racing Multiple Events

`embassy_futures::select::select` lets you await several futures at once and handle whichever completes first. Taking the old board's (board_v1) two key sources as an example:

```rust
// src/event.rs:156-178 (board_v1 variant)
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

Two key sources — whichever is pressed gets handled. `select` does not block on the first wait; it listens to both events simultaneously. On the new board (board_v2) all three keys share one GPIO ladder, so a single `wait_for_falling_edge` suffices and select is not needed (see 4.1).

Likewise in the render task:

```rust
// src/display.rs:144-146
let render_sign = receiver.receive();        // wait for a render request
let quickly_lut = quickly_lut.receive();  // wait for a refresh-mode switch
match select(render_sign, quickly_lut).await {
    // handle whichever arrives first
}
```

### 2.3 Properties of Cooperative Scheduling

Embassy is **cooperative**: tasks only switch at `.await` yield points and are never preempted mid-flight. Two consequences:

- Two stretches of code without `.await` are naturally atomic; ordinary shared data needs no locking (accessing `static mut` still requires `CriticalSectionRawMutex`).
- Tasks do not get their own stacks; they share one stack, and each task itself occupies very little memory.

The price: tasks must conscientiously `.await`. If a task spins without yielding, nothing else ever runs — so every task loop contains an `.await`. Polling BUSY every 2ms during a refresh (see Part 3) and polling the key level every 1ms in the long-press state machine are both examples of waiting while yielding.

## 3. Event System Design

### 3.1 Event Types

The project defines a rich set of event types covering all key interaction patterns:

```rust
// src/event.rs:17-26
pub enum EventType{
    KeyShort(u32),       // short press, parameter is the key number
    KeyLongStart(u32),   // long press start
    KeyLongIng(u32),     // long press in progress (throttled to one enqueue per 100ms)
    KeyLongEnd(u32),     // long press released
    KeyDouble(u32),      // double click
    WheelBack,           // wheel back (reserved)
    WheelFront,          // wheel forward (reserved)
}
```

What an event carries:

```rust
// src/event.rs:28-31
pub struct EventInfo{
    pub ptr:Option<usize>,
}
```

### 3.2 Publish-Subscribe

The heart of the event system is a global listener list:

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

The type signature looks complicated but decomposes clearly:
- **callback**: a closure that receives an `EventInfo` and returns a `Future`
- **event_type**: the event type being listened to
- **ptr**: optional object pointer binding the event to an object
- **fixed**: whether the listener survives `clear()`

### 3.3 Three Registration Flavors

```rust
// One-shot listener (cleared by clear())
pub async fn on<F>(event_type: EventType, callback: F)

// Listener bound to a specific object
pub async fn on_target<F>(event_type: EventType, target_ptr: usize, callback: F)

// Permanent listener (not cleared by clear())
pub async fn on_fixed<F>(event_type: EventType, target_ptr: usize, callback: F)
```

Why three? This is tied to the page system's lifecycle:

1. **`on()`**: registered when a page is entered, cleared in bulk via `clear()` when the page exits. Most listeners use this.

2. **`on_target()`**: same as above, but bound to a specific object pointer so the callback can locate its object via `ptr`.

3. **`on_fixed()`**: permanent listener, unaffected by `clear()`. Used for global functionality (such as sleep detection).

### 3.4 Event Queue and Serial Dispatch

Event production and consumption are **two separate tasks**: the scanner/state machine only produces (enqueues); dispatching is done serially by an independent `dispatch` task.

On the producer side, `toggle_event` only enqueues — it never runs callbacks synchronously:

```rust
// src/event.rs:46-89
/// Key event queue: capacity 8. Producer = toggle_event (scanner/state machine),
/// consumer = the dispatch task.
static EVENT_QUEUE:Mutex<CriticalSectionRawMutex,Vec<EventType,8>>  = Mutex::new(Vec::new()) ;
static EVENT_READY:Signal<CriticalSectionRawMutex,()>  = Signal::new();

/// Enqueue an event (producer side): the scanner/state machine only produces;
/// the dispatch task serializes dispatching — scanning keeps running during a
/// long callback, so no key presses are lost. Repeated KeyLongIng events are
/// merged at the tail; when the queue is full, the oldest is dropped to keep
/// the newest.
pub async fn toggle_event(event_type: EventType,_ms:u64){
    println!("event_type:{:?}",event_type);
    let mut q = EVENT_QUEUE.lock().await;
    if let EventType::KeyLongIng(k) = event_type {
        if let Some(EventType::KeyLongIng(k2)) = q.last() {
            if *k2 == k {
                q.pop(); // same-key KeyLongIng burst → replace in place, no pile-up
            }
        }
    }
    if q.len() >= q.capacity() {
        q.remove(0); // full: drop the oldest, keep the newest
    }
    q.push(event_type).ok();
    drop(q);
    EVENT_READY.signal(());
}
```

On the consumer side, the `dispatch` task waits on the Signal, dequeues FIFO, and dispatches one by one:

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

Three anti-loss / anti-pile-up rules are worth noting:

- **Queue full (capacity 8) drops the oldest, keeps the newest** — when the user taps quickly, the latest intent wins;
- **Same-key `KeyLongIng` tail merging** — long press enqueues only every 100ms, and if the tail is already a `KeyLongIng` for the same key it is replaced in place, so no matter how long the press lasts it occupies a single slot;
- **Serial dispatch** — only one callback runs at any time; callbacks never overlap.

One constraint must be honored: `dispatch_event` executes callbacks **while holding the LISTENER lock**, so **callbacks must not call `on` / `clear` / `on_fixed` to re-register events** — that would grab the same lock again and deadlock. Re-binding still follows the page `run` loop convention: on page switches `bind_event()` runs outside the dispatch task (first `clear()`, then register).

The payoff: while a page callback does slow work (say, page turn = SD read + TTF rasterization for hundreds of milliseconds), the `event::run` scanning task keeps running and new key presses keep being enqueued — unlike the old synchronous-dispatch design, which lost every press during that window.

### 3.5 Binding Events in a Page

The main page illustrates real-world usage:

```rust
// src/pages/main_page.rs:188-229 (excerpt)
async fn bind_event(&mut self){
    event::clear().await;  // clear the previous page's listeners (fixed ones survive)

    // Key2 short press → move cursor down
    event::on(EventType::KeyShort(2),  move |_info|  {
        return Box::pin(async {
            Self::get_mut().await.unwrap().increase();
        });
    }).await;

    // KeyLongEnd(1)/(2): no action on the main page, registered as placeholders
    event::on(EventType::KeyLongEnd(1),  |_info|  { ... }).await;

    // Key1 short press → move cursor up
    event::on(EventType::KeyShort(1),  |_info|  {
        return Box::pin( async {
            Self::get_mut().await.unwrap().decrease();
        });
    }).await;

    // Key3 short press → enter the sub-page (record the page number + persist
    // it so wake-up lands directly on that app)
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

There is a special pattern here: the closure obtains the page's mutable reference via `Self::get_mut().await`. Rust closures cannot directly capture `&mut self` (lifetime issues), so access goes through a global static:

```rust
// src/pages/main_page.rs:132-137
pub async fn get_mut() -> Option<&'static mut MainPage> {
    unsafe {
        let ptr: *mut MainPage =  MAIN_PAGE.lock().await.as_mut().unwrap()  as *mut MainPage;
        Some(&mut *ptr)
    }
}
```

This is an `unsafe` operation, but it is safe under this project's cooperative scheduling — only the dispatch task processes events at any time, so there is no concurrent access.

## 4. Key Detection: From Level to Event

### 4.1 Two Board Variants: Three Keys on One GPIO

In hardware, the three keys share one GPIO through a resistor ladder (`board::KeyLadderGpio`, board-dependent: v1 = GPIO2, v2 = GPIO4): each key connects a different resistance to ground, so pressing different keys produces different voltages:

```
GPIO(ladder) ──┬──[R1]── Key1 ── GND      ← v2: all three keys on the ladder
               ├──[R2]── Key2 ── GND
               └──[R3]── Key3 ── GND
```

- **New board (board_v2)**: all three keys sit on the GPIO4 ladder (internal 45k pull-up). Measured divider values are key1 ≈ 44 / key2 ≈ 409 / key3 ≈ 645 (12-bit), all below the digital low threshold — so any press pulls the level low. A **falling edge** detects "a key was pressed", then the ADC **median** distinguishes which one.
- **Old board (board_v1)**: key1 is wired directly to GPIO9; keys 2/3 sit on the GPIO2 ladder, with the two sources combined by `select` on falling edges (see 2.2); the ladder half still uses the ADC to tell the keys apart.

Both loops call `refresh_active_time()` (refresh the active timestamp) and `sleep::mark_user_touch()` (flag "user touched a key", which exits the timer-wake "render-then-sleep" shortcut) after each key handling round.

### 4.2 The Key State Machine

`key_detection` implements a state machine distinguishing short press, long press and double click:

```rust
// src/event.rs:192-259 (simplified flow)
pub async fn key_detection<P:InputPin>(key: &mut P, key_num: usize)
{
    // KeyLongIng throttle: the loop runs ~1ms per round; unthrottled it would
    // dispatch at ~1kHz and purely burn power.
    const LONG_ING_INTERVAL_MS: u64 = 100;
    let begin_ms = Instant::now().as_millis();
    let mut last_long_ing_ms = begin_ms;
    let mut is_long = false;

    loop {
        // Sample the level 100 times
        let mut is_low_times = 0;
        for _i in 0..100 {
            if key.is_low().unwrap() { is_low_times += 1; }
        }

        if is_low_times > 80 {
            // pressed
            let current = Instant::now().as_millis();
            if current - begin_ms > 500 {
                if !is_long {
                    is_long = true;
                    last_long_ing_ms = current;
                    toggle_event(EventType::KeyLongStart(key_num as u32), current).await;
                } else if current - last_long_ing_ms >= LONG_ING_INTERVAL_MS {
                    last_long_ing_ms = current;  // enqueue at most every 100ms
                    toggle_event(EventType::KeyLongIng(key_num as u32), current).await;
                }
            }
        } else if is_low_times < 2 {
            // released
            let current = Instant::now().as_millis();
            if is_long {
                toggle_event(EventType::KeyLongEnd(key_num as u32), current).await;
                return;
            } else {
                // short press: wait to see whether a double click follows
                loop {
                    let current = Instant::now().as_millis();
                    if !*ENABLE_DOUBLE.lock().await {
                        // double click globally disabled: dispatch as short press
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    if current - begin_ms > 400 {
                        // no second press within 400ms: short press
                        toggle_event(EventType::KeyShort(key_num as u32), current).await;
                        return;
                    }
                    // pressed again → double click
                    if /* 100-sample majority low */ {
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

The state machine, diagrammed:

```
                   press
  [idle] ─────────────► [pressed]
     ▲                    │
     │                    ├── released < 500ms ──► [short-press decision]
     │                    │                      ├── re-press within 400ms ──► KeyDouble
     │                    │                      └── timeout ──► KeyShort
     │                    │
     │                    └── > 500ms ──► [long press]
     │                                      │
     │                                      ├── KeyLongStart (first)
     │                                      ├── KeyLongIng (ongoing, enqueued every 100ms)
     │                                      └── release ──► KeyLongEnd
     │                                                      │
     └──────────────────────────────────────────────────────┘
```

The 100-sample majority debounce (80/100 threshold) is more reliable than classic delay-based debouncing. Every loop iteration yields via `Timer::after(1ms).await`, so other tasks are never blocked.

### 4.3 ADC Key Identification

`judge_adc_num` takes 12 samples and decides on the **median** — medians are naturally immune to transition glitches and the occasional bad sample, and steadier than an average:

```rust
// src/event.rs:265-315 (simplified, board-specific threshold branches)
async fn judge_adc_num() -> usize {
    let mut buf = [0u16; 12];
    for slot in buf.iter_mut() {
        loop {
            let v = { /* hold ADC_PIN/ADC_PER locks, adc.read_oneshot(pin).ok() */ };
            if let Some(v) = v { *slot = v; break; }
            Timer::after_millis(2).await;   // cold boot/channel switches often return Err: retry in 2ms
        }
    }
    buf.sort_unstable();
    let mid = buf[6] as u32;   // median
    println!("[adc_key] press median={}", mid);

    // Key-number decision is board-specific: ladder layout and measured
    // thresholds differ
    #[cfg(feature = "board_v2")]
    {
        // v2 (GPIO4 ladder, key1/2/3 all on the ladder, measured ~44/409/645):
        if mid < 220 { 1 }        // key1 (measured ~44)
        else if mid < 520 { 2 }   // key2 (measured ~409)
        else if mid < 900 { 3 }   // key3 (measured ~645)
        else { 0 }                // well above key3 → treat as glitch, ignore
    }
    #[cfg(feature = "board_v1")]
    {
        // v1 (GPIO2 ladder, only key2/3 on the ladder; key1 is direct-wired to
        // GPIO9 and never enters this function):
        if mid < 200 { 2 }        // key2 (low voltage)
        else if mid > 1000 { 0 }  // glitch, ignore (near the pull-up level)
        else { 3 }                // key3 (higher voltage)
    }
}
```

Sampling must happen immediately after the edge — a fast tap may release within a few tens of milliseconds, so the sample has to beat the release. That is why `judge_adc_num` is the very first thing after the falling edge. A result of 0 (glitch / no key) discards the press and returns to the scan loop after 10ms.

Battery sampling (`battery.rs`) follows the same "retry on error, give up past the limit" strategy (50 tries, keeping the last-known percentage), so sporadic ADC errors never take out key detection or the battery gauge entirely.

## 5. Embassy Synchronization Primitives

The project uses the three synchronization primitives Embassy provides, each in its niche:

### 5.1 Channel: Async Message Passing

For producer-consumer patterns:

```rust
pub static RENDER_CHANNEL: Channel<CriticalSectionRawMutex, RenderInfo, 64> = Channel::new();
```

- Capacity 64, enough to buffer multiple render requests
- Sends do not block (unless full)
- The receiver waits for new messages with `.receive().await`

The event queue is the same idea with a "bounded + drop-oldest" twist: a `Mutex<Vec<EventType, 8>>` + `Signal` combination — smaller capacity but stronger semantics (newest event wins).

### 5.2 Signal: One-Shot Notification

For simple notifications:

```rust
pub static STOP_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
```

A Signal keeps only the latest value, fitting "switch"-style notifications: start WiFi, stop WiFi, reconnect. The event queue's `EVENT_READY` is also a Signal — it only means "something new arrived"; what it is, is the queue's business.

### 5.3 Mutex: Async Mutual Exclusion

For protecting shared data:

```rust
pub static WIFI_INFO:Mutex<CriticalSectionRawMutex,Option<WifiStorage>>  =  Mutex::new(None);
```

`lock().await` returns immediately when the lock is free, otherwise it yields the CPU and waits. Under cooperative scheduling deadlocks cannot happen — as long as every task `.await`s while holding a lock (or avoids long operations while holding one). The one exception is 3.4's rule: dispatch holds the LISTENER lock while running callbacks, so a callback grabbing that same lock deadlocks — the only locking discipline in the project that must be observed manually.

## 6. The Complete Event Flow

The full data path from key press to screen update:

```
Physical key pressed
    │
GPIO level change (falling edge)
    │
event::run() — wait_for_falling_edge().await
    │                 (v2: judge_adc_num median picks the key;
    │                   v1: GPIO9 direct key1, or GPIO2 ladder via select)
    │
key_detection() — debounce/state machine → KeyShort/KeyLong*/KeyDouble
    │
toggle_event() — enqueue into EVENT_QUEUE + EVENT_READY.signal()
    │
event::dispatch task — woken by Signal → FIFO dequeue
    │
dispatch_event() — iterate LISTENER (holding the lock), match event type
    │
Listener callback — mutate page state (e.g. choose_index += 1)
    │
Page render() — draw into the framebuffer
    │
RENDER_CHANNEL.send() — send the render request
    │
display::render() — receive, drive SPI to refresh the panel
    │
EPD shows the new frame
```

Scanning (producer) and dispatch (consumer) are separated by the queue: however slow a callback is, scanning keeps running and no key press is lost.

This chain runs through the project's three core layers — **event layer → page layer → display layer** — each decoupled through Embassy's async mechanisms.

## Summary

| Topic | Key Points |
|-------|------------|
| Embassy executor | `esp_rtos::start` + `#[esp_rtos::main]` |
| Task creation | `#[embassy_executor::task]` + `spawner.spawn()` |
| Cooperative scheduling | `.await` yields the CPU; no preemption, no critical sections |
| select | Await several futures, handle whichever is ready first (v1 dual key sources / render dual Channels) |
| Event system | Queue (capacity 8, drop-oldest, KeyLongIng merging) + serial dispatch task + three registration flavors (on/on_target/on_fixed) |
| Key detection | Edge-triggered + 100-sample debounce + state machine (100ms long-press throttle) + ADC median key identification (per-board threshold sets) |
| Sync primitives | Channel (messages), Signal (notifications), Mutex (shared data) |

Embassy's design philosophy: **use Rust's type system and async/await to guarantee concurrency safety at compile time, and efficient cooperative scheduling at run time**. Embedded development becomes both safe and efficient.

In the next article we enter the world of WiFi, watching how an ESP32-C3 with only 400KB of SRAM runs a full TCP/TLS stack.

---

> Previous: [Part 3: E-Paper Display Driver and Rendering Architecture](03-epd-driver-and-rendering.md) · Next: [Part 5: WiFi Networking and HTTP Communication](../05-WiFi网络与HTTP通信.md)
