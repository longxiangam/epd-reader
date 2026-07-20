# 第 2 篇：硬件抽象与 SPI 总线共享

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

## 引言

操作硬件是嵌入式开发里最容易出错的地方：两个模块同时驱动同一条 SPI 总线、寄存器配置冲突、GPIO 被两边同时拉高拉低——这些问题往往要等到运行时才暴露，调试困难。

esp-hal 用 Rust 的类型系统和所有权把其中一部分变成了编译期错误。本文讲本项目的硬件抽象，重点是最典型的场景：一条 SPI 总线怎么在电子墨水屏和 SD 卡之间安全共享。

## 1. esp-hal v1.x 的硬件抽象设计

### 1.1 外设获取：所有权的第一步

在 PC 上，你可能通过系统调用来操作硬件。但在裸机嵌入式环境中，硬件外设是直接映射到内存地址的寄存器。esp-hal 封装了这些底层细节，提供一个类型安全的接口。

一切始于 `hal_init`：

```rust
// src/main.rs:83-84
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let peripherals = hal_init(config);
```

`hal_init` 返回一个 `Peripherals` 结构体，它包含了 ESP32-C3 的所有硬件外设。关键在于：**每个外设只能被获取一次**。`hal_init` 内部使用了 Rust 的 `take()` 模式——第二次调用会 panic。

这意味着全局只有一个 `Peripherals` 实例，而它的字段通过移动语义确保每个外设只有唯一的所有者。

### 1.2 GPIO：从 Peripherals 到可用的引脚

esp-hal v1.x 中，GPIO 引脚直接从 `Peripherals` 获取：

```rust
// src/main.rs:106-112
let epd_busy = peripherals.GPIO6;          // 输入引脚，直接拿
let epd_rst = peripherals.GPIO7;           // 输出引脚，稍后配置
let epd_cs = Output::new(                  // 输出引脚，立即配置
    peripherals.GPIO3,
    Level::High,                           // 初始电平：高（不选中）
    OutputConfig::default(),               // 默认输出配置
);
```

这里体现了 Rust 的所有权转移：`peripherals.GPIO6` 被"移动"给 `epd_busy` 变量后，`peripherals` 中就不再持有 GPIO6 了。如果后续代码试图再访问 `peripherals.GPIO6`，编译器会报错。

`Output::new` 是 esp-hal v1.x 的新 API，需要三个参数：
- **pin**：GPIO 引脚（所有权转入）
- **initial_level**：初始电平
- **OutputConfig**：输出配置（开漏、上拉等）

其中 `OutputConfig` 是 v1.x 新增的，旧版只需两个参数。这种 Builder 模式的配置结构体在 esp-hal v1.x 中非常普遍。

### 1.3 Input：带配置的输入引脚

```rust
// src/event.rs:99-100
let mut key1 = Input::new(
    key1,
    esp_hal::gpio::InputConfig::default().with_pull(Pull::Up)  // 上拉输入
);
```

按钮引脚配置为上拉输入——未按下时读到高电平，按下时读到低电平。`InputConfig::default().with_pull(Pull::Up)` 是 esp-hal v1.x 的配置方式，比旧版直接传 `Pull::Up` 更具扩展性。

### 1.4 SPI：总线配置

SPI 是本项目最关键的总线，电子墨水屏和 SD 卡都挂在上面：

```rust
// src/main.rs:129-137
let spi = Spi::new(
    peripherals.SPI2,
    SpiConfig::default()
        .with_frequency(Rate::from_mhz(32))   // 32MHz 时钟
        .with_mode(Mode::_0),                  // SPI Mode 0 (CPOL=0, CPHA=0)
).unwrap()
.with_sck(epd_sclk)    // GPIO8  时钟线
.with_miso(epd_miso)   // GPIO10 数据输入
.with_mosi(epd_mosi);  // GPIO0  数据输出
```

esp-hal v1.x 的 SPI 配置使用 `SpiConfig` Builder 模式：
- `with_frequency(Rate::from_mhz(32))`：32MHz 是 SPI2 支持的较高速率
- `with_mode(Mode::_0)`：注意枚举变体用了 PascalCase `_0`，这是 Edition 2024 的规范
- `.with_sck/miso/mosi()`：链式调用分配引脚

**注意**：创建 SPI 实例后，`peripherals.SPI2` 的所有权已经转移，不能再次创建。同时 `epd_sclk`、`epd_miso`、`epd_mosi` 这三个引脚也被消费掉了。

## 2. SPI 总线共享：问题的本质

本项目一条 SPI 总线上挂了两个设备（EPD 和 SD 卡），各自有自己的片选（CS）信号。直觉做法是：操作 SD 卡前拉低 `CS_SD`、传完拉高；操作屏幕前拉低 `CS_EPD`、传完拉高。

问题在于并发：如果两个任务几乎同时操作，MOSI/SCK 线会被两个设备同时驱动，数据错乱。在 Embassy 的异步环境里尤其要注意——一个任务 `.await` 让出执行权时，另一个任务可能正好插进来操作 SPI；一次传输若中途被打断（CS 还拉着、总线却被别人占用）就更糟。

### 2.1 解决方案：CsMutex + CriticalSectionDevice

本项目使用 `critical_section::Mutex` 配合 `embedded_hal_bus::spi::CriticalSectionDevice` 来解决这个问题：

```rust
// src/main.rs:161-168
use critical_section::Mutex as CsMutex;

// 1. 用 CsMutex 包裹 SPI 总线
let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);

// 2. 为每个设备创建独立的 CS-gated SPI 设备
let spi_bus_sd = CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
let spi_bus_epd = CriticalSectionDevice::new_no_delay(shared_spi_static, epd_cs).unwrap();

// 3. 提升为 'static 生命周期
let spi_bus_sd = static_cell::make_static!(spi_bus_sd);
let spi_bus_epd = static_cell::make_static!(spi_bus_epd);
```

工作原理如下图：

```
                    SPI2 (32MHz, Mode 0)
                          │
                    CsMutex<RefCell<Spi>>
                    (临界区互斥锁)
                     ╱          ╲
    CriticalSectionDevice    CriticalSectionDevice
    (CS = GPIO5, SD 卡)      (CS = GPIO3, 屏幕)
         │                         │
    sd_mount 模块              display 模块
```

**CsMutex** 是基于临界区的互斥锁。当任意一个设备要进行 SPI 操作时：
1. 关闭中断（进入临界区）
2. 获取 `RefCell` 的可变引用
3. 拉低自己的 CS 引脚
4. 进行 SPI 数据传输
5. 拉高 CS 引脚
6. 释放引用，恢复中断

`CriticalSectionDevice` 将这个流程封装成一个标准的 `embedded_hal::SpiDevice` trait 实现——对调用者来说，它就是一个普通的 SPI 设备，不需要关心互斥细节。

### 2.2 为什么用 CsMutex 而不是 Embassy Mutex？

这里有一个微妙的设计选择：

| 锁类型 | 实现方式 | 适用场景 |
|--------|---------|---------|
| `CriticalSectionRawMutex` | 关中断 | 短时间持有（微秒级 SPI 操作） |
| `embassy::sync::Mutex` | 异步等待 | 可能长时间持有（网络操作等） |

SPI 操作非常快（几微秒到几十微秒），关中断的开销远小于异步锁的上下文切换开销。而且 SPI 操作本身不能被 `.await` 打断（它不是异步的），所以用临界区锁是最合适的。

### 2.3 `new_no_delay` 是什么？

```rust
CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
```

`new_no_delay` 表示不需要延迟对象。旧版 `CriticalSectionDevice::new()` 需要传入一个 `Delay` 参数，用于 CS 拉低后等待设备就绪。但本项目的 EPD 和 SD 卡都不需要 CS 到数据之间的延迟，所以使用 `new_no_delay` 简化接口。这也是 `embedded-hal-bus` 0.3 版本的改进。

## 3. 全局静态状态管理

在嵌入式 Rust 中，硬件资源通常是全局唯一的——一个 SPI 总线、一个显示控制器、一个 WiFi 接口。但 Rust 的所有权模型要求每个值有唯一的所有者，这与"全局共享"的需求冲突。

本项目使用了四种模式来解决这个矛盾：

### 3.1 模式一：Embassy Mutex + Option

这是最常用的模式，用于需要在多个异步任务间共享的数据：

```rust
// src/wifi.rs 中的示例
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// 使用时
if let Some(wifi) = WIFI_INFO.lock().await.as_ref() {
    println!("wifi_ssid:{:?}", wifi.wifi_ssid);
}

// 写入时
WIFI_INFO.lock().await.replace(wifi_storage);
```

为什么用 `Option` 包装？因为全局变量在声明时还没有数据（硬件还没初始化），所以初始值是 `None`。初始化后通过 `replace()` 填入实际值。

Embassy 的 `Mutex` 是异步的——`lock().await` 会在锁被占用时让出执行权，不会阻塞其他任务。

### 3.2 模式二：core::ptr 安全访问 static mut

用于性能敏感的单写多读场景，如帧缓冲区：

```rust
// src/display.rs:60
static mut DISPLAY: Option<EpdDisplay> = None;

// 写入（仅在初始化时调用一次）
unsafe {
    core::ptr::addr_of_mut!(DISPLAY).write(Some(display));
}

// 读取（在渲染时频繁调用）
pub fn display_mut() -> Option<&'static mut EpdDisplay> {
    unsafe {
        (*core::ptr::addr_of_mut!(DISPLAY)).as_mut()
    }
}
```

为什么不用 Mutex？因为帧缓冲区的访问模式非常明确：
- **写入**：仅在初始化时一次
- **读取**：每次渲染时，从页面代码调用

而且渲染是整个系统最频繁的操作，异步 Mutex 的开销不可接受。使用 `core::ptr::addr_of_mut!` 是 Rust 对 `static mut` 访问的安全要求——直接解引用 `*mut` 在新版编译器中会产生警告甚至错误。

### 3.3 模式三：StaticCell + make_static!

用于将局部变量提升为 `'static` 生命周期：

```rust
// src/main.rs:162-163
let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);
```

`make_static!` 宏将值分配到一个全局静态存储区域，返回 `&'static mut T`。这在 Embassy 任务中很常见——任务的参数通常要求 `'static` 生命周期，但很多值是在 `main()` 中动态创建的。

`static_cell` crate 内部使用一个全局的 `OnceCell` 来确保每个值只被初始化一次。

### 3.4 模式四：RTC 快速内存

用于深度睡眠后需要保持的变量：

```rust
// src/display.rs:47-48
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;
```

ESP32-C3 的 RTC 快速内存是一块特殊的 SRAM 区域，在深度睡眠期间保持供电。放在这里的变量在唤醒后仍然保留上次的值。

`#[ram(unstable(rtc_fast))]` 是 esp-hal v1.x 的写法（旧版是 `#[ram(rtc_fast)]`），需要启用 `unstable` feature。

本项目中有多个 RTC 变量：渲染次数（`RENDER_TIMES`）、电池电量（`LAST_BATTERY_PERCENT`）、睡眠时间戳（`WHEN_SLEEP_RTC_MS`）等——它们都需要在唤醒后恢复状态。

### 四种模式对比

| 模式 | 安全性 | 性能 | 适用场景 |
|------|--------|------|---------|
| Embassy Mutex + Option | 安全（编译期+运行时） | 较低（异步锁） | 多任务共享数据 |
| core::ptr + static mut | unsafe | 最高 | 单写多读、高频访问 |
| StaticCell | 安全 | 一次性 | 局部变量提升为 `'static` |
| RTC 内存 | unsafe | 最高 | 深度睡眠保持 |

## 4. ADC：模拟输入与多按键检测

本项目的三个按键中，按键 2 和按键 3 通过 ADC 分压电路共用一个 GPIO：

```rust
// src/main.rs:143-148
let mut adc1_config = AdcConfig::new();
let adc_pin = unsafe { peripherals.GPIO2.clone_unchecked() };
let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
    adc_pin, Attenuation::_11dB
);
```

硬件上，按键 2 和按键 3 通过不同的电阻分压接到 GPIO2。按下不同的键时，ADC 读到不同的电压值。软件通过采样 ADC 来判断按下的是哪个键：

```rust
// src/event.rs:202-235
async fn judge_adc_num() -> usize {
    // 采样 20 次 ADC 值取平均
    let avg = adc_valute_sum / 20;
    if avg < 200 {
        2      // 按键 2：低电压
    } else {
        3      // 按键 3：较高电压
    }
}
```

esp-hal v1.x 中 ADC 的类型系统更加严格：

```rust
// src/event.rs:119-120
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<esp_hal::peripherals::GPIO2<'static>, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

泛型参数精确到了具体的引脚类型 (`GPIO2<'static>`) 和 ADC 控制器类型 (`ADC1`)，以及校准方式 (`AdcCurve`)。这种精度在旧版中是没有的，它带来了更好的编译期类型安全。

## 5. RTC：实时时钟与睡眠控制

```rust
// src/main.rs:92-93
let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
crate::sleep::RTC_MANGE.lock().await.replace(rtc);
```

RTC（Real-Time Clock）是 ESP32-C3 的低功耗外设，它独立于 CPU 运行，即使在深度睡眠期间也能保持计时。本项目中，RTC 的作用包括：
- 记录进入睡眠的时间，唤醒后通过差值恢复系统时间
- 配置唤醒源（定时器、GPIO）
- 控制深度睡眠

`LPWR`（Low Power）外设在 esp-hal v1.x 中直接从 `Peripherals` 获取，取代了旧版的 `RTC_CNTL`。

## 小结

本文深入了 esp-hal v1.x 的硬件抽象设计：

| 技术点 | 要点 |
|--------|------|
| 外设所有权 | `Peripherals` 的每个字段只能被移动一次 |
| GPIO 新 API | `Output::new(pin, level, OutputConfig)` 增加配置结构体 |
| SPI 配置 | `SpiConfig` Builder 模式，`Rate::from_mhz()` 频率设置 |
| SPI 共享 | `CsMutex` + `CriticalSectionDevice`，临界区互斥 |
| 全局状态 | 四种模式：Embassy Mutex / core::ptr / StaticCell / RTC |
| ADC | 严格泛型参数，曲线校准，模拟按键检测 |
| RTC | `LPWR` 外设，低功耗计时，深度睡眠控制 |

核心思想是：**用 Rust 的类型系统在编译期保证硬件访问的安全性，用运行时锁在必要时协调并发访问**。

在下一篇文章中，我们将进入电子墨水屏的世界，看看显示渲染架构是如何设计的。

---

> 上一篇：[第 1 篇：项目全景](01-项目全景.md) · 下一篇：[第 3 篇：电子墨水屏驱动与渲染架构](03-电子墨水屏驱动与渲染架构.md)
