# 第 10 篇：esp-hal v0.19 → v1.x 迁移实战

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

## 引言

esp-hal 是 ESP32 Rust 开发的核心 crate。从 v0.19 到 v1.x 是一个主版本升级，API 几乎全面重写。本项目在 2025 年初完成了这次迁移，涉及 15+ 个 crate 的版本升级和几乎每个源文件的代码修改。

本文将以本项目的实际迁移经验为基础，总结迁移中遇到的关键变化和解决方案，希望能帮助其他开发者少走弯路。

## 1. 依赖迁移全景

### 1.1 版本对照表

| 功能 | 旧版 | 新版 | 说明 |
|------|------|------|------|
| HAL | esp-hal 0.19 | esp-hal 1.1.0 | API 全面重写 |
| WiFi | esp-wifi 0.7 | esp-radio 0.18 | 驱动独立为单独 crate |
| Embassy 集成 | esp-hal-embassy 0.2 | esp-rtos 0.3 | 统一运行时入口 |
| 内存分配 | embedded-alloc | esp-alloc 0.10 | ESP 专用宏 |
| 引导 | *(无)* | esp-bootloader-esp-idf 0.5 | IDF 兼容描述 |
| 网络 | embassy-net 0.4 | embassy-net 0.9 | Stack/Runner 分离 |
| 执行器 | embassy-executor 0.6 | embassy-executor 0.10 | API 微调 |
| 同步 | embassy-sync 0.6 | embassy-sync 0.8 | API 微调 |
| 时间 | embassy-time 0.3 | embassy-time 0.5 | API 微调 |
| SPI 共享 | embedded-hal-bus 0.1 | embedded-hal-bus 0.3 | new_no_delay |
| HTTP | reqwless 0.11 | reqwless 0.14 | Response API 变化 |
| 网络地址 | esp_wifi::wifi::ipv4 | no-std-net 0.6 | 独立 crate |
| Rust 版本 | Edition 2021 | Edition 2024 | 枚举命名规范变化 |

### 1.2 消除 patches

旧版项目需要在 `Cargo.toml` 中用 `[patch.crates-io]` 来修复上游 crate 的兼容性问题。esp-hal v1.x 生态成熟后，这些补丁全部可以移除，直接使用上游 crate。

## 2. 入口与初始化迁移

### 2.1 程序入口

```rust
// 旧版
#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // ...
}

// 新版
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // ...
}
```

变化：
- 入口宏从 `esp_hal_embassy::main` 变为 `esp_rtos::main`
- 新增 `esp_app_desc!()` 宏生成 IDF 兼容的应用描述
- 返回类型变为显式 `-> !`（永不返回）

### 2.2 HAL 初始化

```rust
// 旧版：多步初始化
let peripherals = Peripherals::take();
let system = SystemControl::new(peripherals.SYSTEM);
let clocks = ClockControl::max(system.clock_control).freeze();

// 新版：一步完成
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let peripherals = hal_init(config);
```

新版不再需要 `SystemControl`、`ClockControl`、`clocks` 引用。`hal_init` 返回配置好的 `Peripherals`。

### 2.3 Embassy 启动

```rust
// 旧版
esp_hal_embassy::init(&clocks, systimer.alarm0);

// 新版
let timg0 = TimerGroup::new(peripherals.TIMG0);
let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);
```

不再需要 `&clocks` 引用。使用硬件定时器（TIMG0）和软件中断的组合来驱动 Embassy 调度器。

### 2.4 内存分配器

```rust
// 旧版
use embedded_alloc::Heap;
#[global_allocator]
static HEAP: Heap = Heap::empty();
fn init_heap() {
    unsafe {
        HEAP.init(heap_start as usize, 80 * 1024);
    }
}

// 新版
esp_alloc::heap_allocator!(size: 80 * 1024);
```

从手动初始化 `embedded_alloc::Heap` 到一行宏搞定。堆大小也从 38KB 增加到了 80KB。

## 3. GPIO 迁移

```rust
// 旧版
let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
let epd_busy = io.pins.gpio6;
let epd_cs = Output::new(io.pins.gpio3, Level::High);

// 新版
let epd_busy = peripherals.GPIO6;
let epd_cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
```

变化：
- 不再需要中间 `Io` 对象
- 引脚直接从 `Peripherals` 获取
- `Output::new` 增加 `OutputConfig` 参数
- GPIO 类型从 `Gpio6` 变为 `esp_hal::peripherals::GPIO6<'static>`

Input 类似：

```rust
// 旧版
Input::new(pin, Pull::Up)

// 新版
Input::new(pin, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up))
```

## 4. SPI 迁移

```rust
// 旧版
let spi = Spi::new(peripherals.SPI2, 32u32.MHz(), SpiMode::Mode0, &clocks);

// 新版
let spi = Spi::new(
    peripherals.SPI2,
    SpiConfig::default()
        .with_frequency(Rate::from_mhz(32))
        .with_mode(Mode::_0),
).unwrap();
```

变化：
- 频率从 `32u32.MHz()` 变为 `Rate::from_mhz(32)`
- 模式从 `SpiMode::Mode0` 变为 `Mode::_0`（Edition 2024 PascalCase）
- 不再需要 `&clocks` 参数
- 使用 `SpiConfig` Builder 模式

SPI 共享：

```rust
// 旧版
CriticalSectionDevice::new(&shared_spi, cs_pin, delay)

// 新版
CriticalSectionDevice::new_no_delay(shared_spi_static, cs_pin).unwrap()
```

不再需要 `Delay` 参数。

## 5. WiFi 迁移

这是变化最大的模块。

### 5.1 初始化

```rust
// 旧版：需要大量手动初始化
let init = esp_wifi::initialize(
    EspWifiInitFor::Wifi,
    timer, rng, radio_clk, &clocks
)?;
let (interface, controller) = esp_wifi::wifi::new_with_mode(
    &init, wifi, WifiStaDevice
);

// 新版：一行完成
let (controller, interfaces) = esp_radio::wifi::new(
    wifi,
    ControllerConfig::default().with_initial_config(station_config),
)?;
let wifi_interface = interfaces.station;
```

旧版需要传入 `timer`、`rng`、`radio_clk`、`&clocks` 四个参数。新版只需要 `wifi` 外设和配置。

### 5.2 网络栈

```rust
// 旧版
let stack = Stack::new(interface, config, resources, seed);
spawn(net_task(&stack));

// 新版
let (stack, runner) = embassy_net::new(interface, config, resources, seed);
spawn(net_task(runner));
```

`Stack` 和 `Runner` 分离——Runner 作为独立任务运行网络协议栈，Stack 只作为接口。泛型也从 `Stack<WifiDevice<'static, WifiStaDevice>>` 简化为 `Stack<'static>`。

### 5.3 WiFi 事件

```rust
// 旧版
controller.wait_for_event(WifiEvent::StaDisconnected).await;

// 新版
let mut subscriber = controller.subscribe().unwrap();
subscriber.next_event_pure().await;
```

新版使用 subscribe/next_event 模式，更灵活但需要手动判断事件类型。

### 5.4 UDP API

```rust
// 旧版
let (n, src) = udp_socket.recv_from(&mut buf).await;
// src 是 IpEndpoint

// 新版
let (n, src) = udp_socket.recv_from(&mut buf).await;
// src 是 UdpMetadata，通过 src.endpoint 获取地址
```

## 6. 其他迁移点

### 6.1 RNG

```rust
// 旧版
let rng = Rng::new(peripherals.RNG);

// 新版
let rng = Rng::new();  // 不需要参数
```

### 6.2 RTC 内存

```rust
// 旧版
#[ram(rtc_fast)]

// 新版
#[ram(unstable(rtc_fast))]  // 需要 unstable feature
```

### 6.3 static mut 访问

```rust
// 旧版
unsafe { DISPLAY.replace(display); }

// 新版
unsafe { core::ptr::addr_of_mut!(DISPLAY).write(Some(display)); }
```

编译器对 `static mut` 的直接访问更加严格，新版使用 `core::ptr::addr_of_mut!` 避免未定义行为。

### 6.4 Flash

```rust
// 旧版
let mut flash = FlashStorage::new();

// 新版
let flash = unsafe { esp_hal::peripherals::FLASH::steal() };
let mut flash = FlashStorage::new(flash);
```

### 6.5 网络地址类型

```rust
// 旧版
use esp_wifi::wifi::ipv4::*;

// 新版
use no_std_net::{IpAddr, Ipv4Addr, SocketAddr};
```

网络地址类型从 WiFi 库中独立出来，不再依赖 WiFi crate。

### 6.6 异步 trait

```rust
// 旧版
async fn send_to(&self, buf: &[u8], addr: T) -> Result<usize> { ... }

// 新版
fn send_to(&self, buf: &[u8], addr: T) -> impl Future<Output = Result<usize>> { ... }
```

Edition 2024 中 `impl Trait` 在更多位置可用，`async fn` 在 trait 中的写法也有了变化。

### 6.7 Backtrace

```rust
// 旧版
let frames: [Option<u32>; 10] = esp_backtrace::arch::backtrace();

// 新版
let backtrace = Backtrace::capture();
for frame in backtrace.frames() {
    frame.program_counter()
}
```

结构化的 Backtrace 对象取代了裸数组。

## 7. 迁移策略建议

### 7.1 逐步迁移 vs 一次性迁移

本项目选择了一次性迁移——因为 esp-hal v1.x 的变化太大，逐步迁移中间状态的代码很难编译通过。建议：

1. 创建新分支
2. 先更新 `Cargo.toml` 的所有依赖版本
3. 解决编译错误（会很多）
4. 逐模块测试

### 7.2 最容易踩的坑

| 坑 | 原因 | 解决方案 |
|---|------|---------|
| GPIO 类型不匹配 | 类型从 `Gpio6` 变为 `GPIO6<'static>` | 全局搜索替换 |
| `&clocks` 引用 | 旧版到处传递 clocks 引用 | 新版不需要，删除 |
| 枚举命名 | `Mode0` → `Mode::_0` | Edition 2024 PascalCase |
| Stack 泛型简化 | `Stack<WifiDevice<...>>` → `Stack<'static>` | 删除泛型参数 |
| `new_no_delay` | 旧版 `CriticalSectionDevice::new()` 需要 Delay | 改用 `new_no_delay()` |

### 7.3 Edition 2024 注意事项

- 枚举变体命名规范变化（`Mode0` → `Mode::_0`）
- `impl Trait` 在 trait 关联类型中可用
- 部分生命周期推断规则调整

## 8. 迁移收益

| 方面 | 改善 |
|------|------|
| 代码简洁性 | 初始化代码从 ~20 行减少到 ~5 行 |
| 类型安全 | 更严格的泛型参数，编译期捕获更多错误 |
| 依赖管理 | 不再需要 `[patch.crates-io]` |
| API 一致性 | 所有 HAL 组件使用统一的 Config Builder 模式 |
| 社区支持 | v1.x 是当前维护版本，问题修复更及时 |

## 小结

esp-hal v0.19 → v1.x 的迁移工作量不小，但收益也很明显。核心变化可以总结为三点：

1. **初始化简化**：从多步手动配置到一行声明式配置
2. **依赖解耦**：WiFi 驱动、网络类型、内存分配器各自独立
3. **类型强化**：更严格的泛型参数，编译期能发现更多问题

迁移的核心原则是：**相信编译器**——把旧代码注释掉，让编译器告诉你哪里需要改。Rust 的类型系统会引导你走到正确的方向。

---

> 上一篇：[第 9 篇：错误处理与调试](09-错误处理与调试.md) · [返回目录](README.md)
