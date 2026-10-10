# 第 2 篇：硬件抽象与 SPI 总线共享

> English: [en/02-hardware-abstraction-and-spi-bus.md](en/02-hardware-abstraction-and-spi-bus.md)
>
> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

## 引言

操作硬件是嵌入式开发里最容易出错的地方：两个模块同时驱动同一条 SPI 总线、寄存器配置冲突、GPIO 被两边同时拉高拉低——这些问题往往要等到运行时才暴露，调试困难。

esp-hal 用 Rust 的类型系统和所有权把其中一部分变成了编译期错误。本文讲本项目的硬件抽象：先用 feature 组合出两代板型的板级抽象，再看一条 SPI 总线怎么在电子墨水屏和 SD 卡之间安全共享、一条 I2C 总线怎么挂 PCF8563 时钟和 AHT20 温湿度传感器，最后是嵌入式 Rust 中全局状态管理的几种模式。

## 1. esp-hal v1.x 的硬件抽象设计

### 1.1 外设获取：所有权的第一步

在 PC 上，你可能通过系统调用来操作硬件。但在裸机嵌入式环境中，硬件外设是直接映射到内存地址的寄存器。esp-hal 封装了这些底层细节，提供一个类型安全的接口。

一切始于 `hal_init`：

```rust
// src/main.rs:109-110
let config = HalConfig::default().with_cpu_clock(CpuClock::max());
let mut peripherals = hal_init(config);
```

`hal_init` 返回一个 `Peripherals` 结构体，它包含了 ESP32-C3 的所有硬件外设。关键在于：**每个外设只能被获取一次**。`hal_init` 内部使用了 Rust 的 `take()` 模式——第二次调用会 panic。

这意味着全局只有一个 `Peripherals` 实例，而它的字段通过移动语义确保每个外设只有唯一的所有者。注意 `peripherals` 是可变绑定：接下来的板级初始化函数要拿 `&mut Peripherals` 去配置引脚（见第 2 节）。

### 1.2 GPIO：从 Peripherals 到可用的引脚

esp-hal v1.x 中，GPIO 引脚直接从 `Peripherals` 获取：

```rust
// src/main.rs:152-160
let epd_busy = peripherals.GPIO6;          // 输入引脚，直接拿
let epd_rst = peripherals.GPIO7;           // 稍后包装成 Output
let epd_dc = peripherals.GPIO20;
let epd_cs = Output::new(                  // 输出引脚，立即配置
    peripherals.GPIO3,
    Level::High,                           // 初始电平：高（不选中）
    OutputConfig::default(),               // 默认输出配置
);
let sdcard_cs = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
```

这里体现了 Rust 的所有权转移：`peripherals.GPIO6` 被"移动"给 `epd_busy` 变量后，`peripherals` 中就不再持有 GPIO6 了。如果后续代码试图再访问 `peripherals.GPIO6`，编译器会报错。

`Output::new` 是 esp-hal v1.x 的新 API，需要三个参数：
- **pin**：GPIO 引脚（所有权转入）
- **initial_level**：初始电平
- **OutputConfig**：输出配置（开漏、上拉等）

其中 `OutputConfig` 是 v1.x 新增的，旧版只需两个参数。这种 Builder 模式的配置结构体在 esp-hal v1.x 中非常普遍。

### 1.3 Input：带配置的输入引脚

```rust
// src/event.rs:129（v2 按键扫描任务内）
let mut key_all = Input::new(key2, esp_hal::gpio::InputConfig::default().with_pull(Pull::Up));
```

按钮引脚配置为上拉输入——未按下时读到高电平，按下时读到低电平。`InputConfig::default().with_pull(Pull::Up)` 是 esp-hal v1.x 的配置方式，比旧版直接传 `Pull::Up` 更具扩展性。

有个细节：按键梯引脚同时用作 ADC（区分键号）。ADC 的 `set_analog` 会剥掉数字配置，所以 `main` 里在 ADC 初始化之后要对同一个脚再配一次数字上拉输入：

```rust
// src/main.rs:216-220
let k2_in = Input::new(
    crate::board::key_ladder_pin(),
    InputConfig::default().with_pull(Pull::Up),
);
core::mem::forget(k2_in);   // 只需配置生效，句柄不保留
```

### 1.4 SPI：总线配置

SPI 是本项目最关键的总线，电子墨水屏和 SD 卡都挂在上面：

```rust
// src/main.rs:163-172
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

另一个初始化顺序的细节：MISO（GPIO10）要在 SPI 占用它之前先配上内部上拉——SD 卡未初始化前 DO 是高阻态，没有上拉时 CMD0 的应答读不可靠：

```rust
// src/board/mod.rs
pub fn sd_miso_pullup(p: &mut esp_hal::peripherals::Peripherals) {
    let miso_pu = Input::new(
        unsafe { p.GPIO10.clone_unchecked() },
        InputConfig::default().with_pull(Pull::Up),
    );
    core::mem::forget(miso_pu); // 配置保持到整个运行期
}
```

## 2. 板级抽象：board_v1 / board_v2

项目有两代硬件，`src/board/` 用三个文件把它们抽象成统一接口：

- **`mod.rs`**：屏型/板型守卫（`compile_error!`）+ 两代板相同的 EPD/SPI/SD 常量 + 公共助手 + 诊断
- **`v1.rs`**：旧板（可配 2.9 / 2.7 / 4.2 三种屏）
- **`v2.rs`**：v22 新板（仅 4.2 寸屏）

feature 拆成两个维度：`epd_*` 选屏幕驱动、`board_*` 选板型，一个设备 = epd + board 组合。选中的板文件被 `pub use` 统一再导出，调用方一律写 `board::xxx`，不感知具体板文件：

```rust
// src/board/mod.rs
#[cfg(feature = "board_v1")]
pub mod v1;
#[cfg(feature = "board_v2")]
pub mod v2;
#[cfg(feature = "board_v1")]
pub use v1::*;
#[cfg(feature = "board_v2")]
pub use v2::*;
```

编译矩阵：

| feature 组合 | 设备 |
|--------------|------|
| `epd4in2,board_v2` | v22 新板，4.2 寸 |
| `epd4in2,board_v1` | 旧板，4.2 寸 |
| `epd2in7,board_v1` | 旧板，2.7 寸 |
| `epd2in9,board_v1` | 旧板，2.9 寸 |

两代板的 EPD/SPI/SD 引脚相同，定义在 `mod.rs`：

```rust
// src/board/mod.rs —— EPD / SPI / SD（两代板相同）
pub const EPD_BUSY_GPIO: u8 = 6;
pub const EPD_RST_GPIO: u8 = 7;
pub const EPD_DC_GPIO: u8 = 20;
pub const EPD_CS_GPIO: u8 = 3;
pub const SPI_SCLK_GPIO: u8 = 8;
pub const SPI_MOSI_GPIO: u8 = 0;
pub const SPI_MISO_GPIO: u8 = 10;
pub const SD_CS_GPIO: u8 = 5;
```

板上差异全部收敛在 `v1.rs` / `v2.rs` 里：

| | board_v1（旧板） | board_v2（v22 新板） |
|------|------------------|----------------------|
| 三键 | 键1=GPIO9 数字直连；键2/3=GPIO2 ADC 梯 | 键1/2/3 全在 GPIO4 ADC 梯（兼深睡唤醒） |
| 电池分压 ADC | GPIO4 | GPIO2 |
| I2C | 无 | GPIO9=SDA / GPIO21=SCL，挂 PCF8563(0x51) + AHT20(0x38) |
| 电源轨 | 双轨：EINK=GPIO21 / SD=GPIO1，**高电平=断电** | 单轨 GPIO1，**低电平=开启** |
| 深睡 pad hold | 给 GPIO1（SD 轨）打 hold | 清 `pad_hold` 寄存器，释放上次深睡布下的 hold |
| `HAS_PCF` / `HAS_AHT20` | false / false | true / true |

每个板文件提供同名 API，`main` 只面对 `board::` 前缀调用：

| 函数 | 作用 |
|------|------|
| `init_pad_hold(&mut Peripherals)` | 深睡 pad hold（v1 打 hold / v2 释放 hold） |
| `init_i2c(&mut Peripherals) -> Option<I2c>` | v2 构造 I2C 总线，v1 返回 `None` |
| `init_power_rails(&mut Peripherals)` | 构造电源轨输出脚，填进 sleep 模块的 `EINK_PWER_PIN` / `SD_PWER_PIN` 两槽 |
| `key_ladder_pin()` | 三键梯 GPIO（ADC + 数字输入 + 深睡唤醒共用一 pad） |
| `key_direct_pin()` | 数字直连键（仅 v1 有） |
| `bat_pin()` | 电池分压 ADC 引脚 |
| `spawn_sensors(&Spawner)` | v2 spawn AHT20 采样任务，v1 空实现 |
| `print_boot_diag()` | rtc_fast 自增诊断（跨深睡递增 = 保留正常） |

电源轨是板间差异的典型例子。旧板双轨、高电平断电，新板单轨、低电平开启，但对外都是"往两个槽里填电源控制脚"：

```rust
// src/board/v1.rs —— 双轨（高=断），初始化后立即拉低上电
pub async fn init_power_rails(p: &mut Peripherals) {
    let mut eink_pwr_ctrl = Output::new(unsafe { p.GPIO21.clone_unchecked() }, Level::High, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::High, OutputConfig::default());
    eink_pwr_ctrl.set_low();
    sd_pwr_ctrl.set_low();
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}

// src/board/v2.rs —— 单轨（低=开），EINK/SD 两槽填同一轨，睡眠时统一置高断轨
pub async fn init_power_rails(p: &mut Peripherals) {
    let mut eink_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    let mut sd_pwr_ctrl = Output::new(unsafe { p.GPIO1.clone_unchecked() }, Level::Low, OutputConfig::default());
    crate::sleep::EINK_PWER_PIN.lock().await.replace(eink_pwr_ctrl);
    crate::sleep::SD_PWER_PIN.lock().await.replace(sd_pwr_ctrl);
}
```

### `Peripherals::steal()`：晚期取脚

按键、电池这些引脚在 `main` 里初始化得比较晚——那时 EPD/SPI 相关字段已经从 `peripherals` 里 move 出去了，没法再借 `&mut Peripherals`。板级文件统一走 `Peripherals::steal()` + `clone_unchecked()` 再取一次（与全工程 `clone_unchecked` 的惯例一致；单核、这段代码只在启动时跑一遍，是安全的）：

```rust
// src/board/v2.rs
pub type KeyLadderGpio = esp_hal::peripherals::GPIO4<'static>;
pub type BatAdcGpio = esp_hal::peripherals::GPIO2<'static>;

pub fn key_ladder_pin() -> KeyLadderGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO4.clone_unchecked() }
}
pub fn bat_pin() -> BatAdcGpio {
    unsafe { esp_hal::peripherals::Peripherals::steal().GPIO2.clone_unchecked() }
}
```

类型别名 `KeyLadderGpio` / `BatAdcGpio` 还有一个作用：ADC 静态变量的泛型参数引用它们，引脚换板时类型自动跟着走（见第 6 节）。

## 3. SPI 总线共享：问题的本质

本项目一条 SPI 总线上挂了两个设备（EPD 和 SD 卡），各自有自己的片选（CS）信号。直觉做法是：操作 SD 卡前拉低 `CS_SD`、传完拉高；操作屏幕前拉低 `CS_EPD`、传完拉高。

问题在于并发：如果两个任务几乎同时操作，MOSI/SCK 线会被两个设备同时驱动，数据错乱。在 Embassy 的异步环境里尤其要注意——一个任务 `.await` 让出执行权时，另一个任务可能正好插进来操作 SPI；一次传输若中途被打断（CS 还拉着、总线却被别人占用）就更糟。

### 3.1 解决方案：CsMutex + CriticalSectionDevice

本项目使用 `critical_section::Mutex` 配合 `embedded_hal_bus::spi::CriticalSectionDevice` 来解决这个问题：

```rust
// src/main.rs:260-268
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

### 3.2 为什么用 CsMutex 而不是 Embassy Mutex？

这里有一个微妙的设计选择：

| 锁类型 | 实现方式 | 适用场景 |
|--------|---------|---------|
| `CriticalSectionRawMutex` | 关中断 | 短时间持有（微秒级 SPI 操作） |
| `embassy::sync::Mutex` | 异步等待 | 可能长时间持有（网络操作等） |

SPI 操作非常快（几微秒到几十微秒），关中断的开销远小于异步锁的上下文切换开销。而且 SPI 操作本身不能被 `.await` 打断（它不是异步的），所以用临界区锁是最合适的。

### 3.3 `new_no_delay` 是什么？

```rust
CriticalSectionDevice::new_no_delay(shared_spi_static, sdcard_cs).unwrap();
```

`new_no_delay` 表示不需要延迟对象。旧版 `CriticalSectionDevice::new()` 需要传入一个 `Delay` 参数，用于 CS 拉低后等待设备就绪。但本项目的 EPD 和 SD 卡都不需要 CS 到数据之间的延迟，所以使用 `new_no_delay` 简化接口。这也是 `embedded-hal-bus` 0.3 版本的改进。

## 4. I2C 总线共享与板载外设

I2C 是 v2 新板引入的第二条共享总线，封装在 `src/i2c_bus.rs`。与 SPI 的 CsMutex 方案类似——静态 `Mutex` 包裹 `Option<I2c>`，`None` 表示总线上没有设备（旧板）：

```rust
// src/i2c_bus.rs
pub type I2cDev = I2c<'static, esp_hal::Blocking>;

pub static I2C: Mutex<CriticalSectionRawMutex, Option<I2cDev>> = Mutex::new(None);

pub async fn write(addr: u8, data: &[u8]) -> Result<(), ()> { ... }
pub async fn write_read(addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ()> { ... }
```

驱动不直接碰 `I2C` 静态量，而是经 `write` / `write_read` 助手访问总线，错误统一收敛成 `Result<(), ()>`。总线由 `board::init_i2c()` 构造（GPIO9=SDA / GPIO21=SCL，`Config::default()` 即 100kHz——PCF8563 的安全上限），挂两个设备：

- **AHT20（0x38）**：室内温湿度传感器，`aht20::sample_task` 周期采样，由 `board::spawn_sensors` 启动
- **PCF8563（0x51）**：硬件 RTC，CR2032 纽扣电池备电，角色是**系统时钟源**

PCF8563 的时间以 UTC 存取（BCD 编码，`Clock::local` 负责时区）：

- **写入**：NTP 对时成功后 `write_time(ts)` 把 UTC 时间写进去（先置 STOP 暂停走时防撕裂，写完清 STOP）
- **读取**：每次唤醒/重启优先从它恢复时间。`read_time()` 校验 VL 无效标志（备电掉过电则返回 None）和字段合法性；`ntp_worker` 与阅读模式的 `init_clock_pcf` 都先走它，失败再退回 rtc_fast 估算
- 深睡漂移、阅读模式无 WiFi、断电换电池这些场景都不再丢时间

它还承担**深睡定时唤醒**：倒计时定时器（`arm_timer_secs` / `arm_timer_minutes`，1Hz 或 1/60Hz 分频，最长 255 分钟）到点把 INT 脚拉低，经 GPIO4 唤醒 CPU；更长的间隔用绝对时间报警 `arm_alarm_at`（PCF 自身走时，比主控深睡慢时钟精确）。唤醒后 `consume_timer_wake()` 读 TF/AF 标志判断是否定时唤醒，并清标志 + 关定时器——INT 若一直拉着 GPIO4，运行期会把按键网络拉低造成假按键。

## 5. 全局静态状态管理

在嵌入式 Rust 中，硬件资源通常是全局唯一的——一个 SPI 总线、一个显示控制器、一个 WiFi 接口。但 Rust 的所有权模型要求每个值有唯一的所有者，这与"全局共享"的需求冲突。

本项目使用了四种模式来解决这个矛盾：

### 5.1 模式一：Embassy Mutex + Option

这是最常用的模式，用于需要在多个异步任务间共享的数据：

```rust
// src/storage.rs
pub static WIFI_INFO: Mutex<CriticalSectionRawMutex, Option<WifiStorage>> = Mutex::new(None);

// 使用时（src/wifi.rs）
if let Some(wifi) = crate::storage::WIFI_INFO.lock().await.as_ref() {
    println!("wifi_ssid:{:?}", wifi.wifi_ssid);
}

// 写入时
WIFI_INFO.lock().await.replace(wifi);
```

为什么用 `Option` 包装？因为全局变量在声明时还没有数据（硬件还没初始化），所以初始值是 `None`。初始化后通过 `replace()` 填入实际值。

Embassy 的 `Mutex` 是异步的——`lock().await` 会在锁被占用时让出执行权，不会阻塞其他任务。前文的 `SPI` 总线容器（CsMutex）、I2C 容器（`i2c_bus::I2C`）、ADC 容器（`event::ADC_PER`）、电源轨槽（`sleep::EINK_PWER_PIN`）都是这个模式。

### 5.2 模式二：core::ptr 安全访问 static mut

用于性能敏感的单写多读场景，如帧缓冲区：

```rust
// src/display.rs
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

### 5.3 模式三：StaticCell + make_static!

用于将局部变量提升为 `'static` 生命周期：

```rust
// src/main.rs:261-262
let shared_spi = CsMutex::new(RefCell::new(spi));
let shared_spi_static = static_cell::make_static!(shared_spi);
```

`make_static!` 宏将值分配到一个全局静态存储区域，返回 `&'static mut T`。这在 Embassy 任务中很常见——任务的参数通常要求 `'static` 生命周期，但很多值是在 `main()` 中动态创建的。

`static_cell` crate 内部使用一个全局的 `OnceCell` 来确保每个值只被初始化一次。

### 5.4 模式四：RTC 快速内存

用于深度睡眠后需要保持的变量：

```rust
// src/display.rs
#[ram(unstable(rtc_fast))]
static mut RENDER_TIMES: u32 = 0;
```

ESP32-C3 的 RTC 快速内存是一块特殊的 SRAM 区域，在深度睡眠期间保持供电。放在这里的变量在唤醒后仍然保留上次的值。

`#[ram(unstable(rtc_fast))]` 是 esp-hal v1.x 的写法（旧版是 `#[ram(rtc_fast)]`），需要启用 `unstable` feature。

本项目中有多个 RTC 变量：渲染次数（`RENDER_TIMES`）、冷启动清屏标记（`NEED_CLEAR`）、电池电量（`LAST_BATTERY_PERCENT`）、睡眠时间戳（`WHEN_SLEEP_RTC_MS`）、启动序号（`board::RTC_FAST_BOOT_SEQ` 诊断用）等——它们都需要在唤醒后恢复状态。

注意 rtc_fast 的边界：它**只跨深睡保留**，跨不上电、跨不了软件复位；v22 上 IO（RTCIO）唤醒也可能丢内容。所以关键状态要有 NVS 后备——比如"上次在阅读"的 `READING_MODE` 标志落 NVS，唤醒后从 NVS 恢复， rtc_fast 只当快路径。

### 四种模式对比

| 模式 | 安全性 | 性能 | 适用场景 |
|------|--------|------|---------|
| Embassy Mutex + Option | 安全（编译期+运行时） | 较低（异步锁） | 多任务共享数据 |
| core::ptr + static mut | unsafe | 最高 | 单写多读、高频访问 |
| StaticCell | 安全 | 一次性 | 局部变量提升为 `'static` |
| RTC 内存 | unsafe | 最高 | 深度睡眠保持 |

## 6. ADC：模拟输入与多按键检测

三个按键走 ADC 分压梯：按下不同按键时，分压中点电压不同，ADC 采样值就不同。引脚由板级抽象给出——v2 全部三键在 GPIO4 梯上，v1 只有键2/3 在 GPIO2 梯上（键1 走 GPIO9 直连）：

```rust
// src/main.rs:199-212
let mut adc1_config = AdcConfig::new();
// 按键梯：v2=GPIO4 / v1=GPIO2
let adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
    crate::board::key_ladder_pin(), Attenuation::_11dB,
);
// 电池分压：v2=GPIO2 / v1=GPIO4
let bat_adc1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<esp_hal::peripherals::ADC1>>(
    crate::board::bat_pin(), Attenuation::_11dB,
);

let adc1 = Adc::new(peripherals.ADC1, adc1_config);
event::ADC_PER.lock().await.replace(adc1);
event::ADC_PIN.lock().await.replace(adc1_pin);
```

esp-hal v1.x 中 ADC 的类型系统非常严格：

```rust
// src/event.rs
pub static ADC_PIN: Mutex<CriticalSectionRawMutex, Option<
    AdcPin<crate::board::KeyLadderGpio, esp_hal::peripherals::ADC1, AdcCalCurve<esp_hal::peripherals::ADC1>>
>> = Mutex::new(None);

pub static ADC_PER: Mutex<CriticalSectionRawMutex, Option<
    Adc<'static, esp_hal::peripherals::ADC1, esp_hal::Blocking>
>> = Mutex::new(None);
```

泛型参数精确到了具体的引脚类型和 ADC 控制器类型以及校准方式。关键在于引脚类型引用的是板级别名 `crate::board::KeyLadderGpio`（v2=GPIO4 / v1=GPIO2）——换板只改 board 文件，事件模块的静态量类型自动跟着变。这种精度在旧版中是没有的，它带来了更好的编译期类型安全。

### 6.1 键号判定：中位数滤波 + 板型阈值

`judge_adc_num()` 连续采样 12 次取中位数（抗过渡期毛刺），再按板型阈值判定键号：

```rust
// src/event.rs
#[cfg(feature = "board_v2")]
{
    // v2（GPIO4 梯，键1/2/3 都在梯上，实测 ≈25/409/645）：
    // <220→键1；220~520→键2；520~900→键3；更高视为抖动忽略
    if mid < 220 { 1 } else if mid < 520 { 2 } else if mid < 900 { 3 } else { 0 }
}
#[cfg(feature = "board_v1")]
{
    // v1（GPIO2 梯，只有键2/3 在梯上，键1 走 GPIO9 直连不进此函数）：
    // <200→键2；200~1000→键3；>1000 抖动忽略
    if mid < 200 { 2 } else if mid > 1000 { 0 } else { 3 }
}
```

### 6.2 按键扫描任务的两个板型变体

`event::run` 同样按板型各写一份。v2 三键共用 GPIO4，按下都会把脚拉到数字低电平（实测三键分压均低于数字低阈值）——所以用下降沿检测"有没有按"，ADC 中位数只负责区分"按的哪个"；边沿后立即采样，因为快速点按可能在几十 ms 内松开：

```rust
// src/event.rs（v2）
#[embassy_executor::task]
pub async fn run(key2: crate::board::KeyLadderGpio) {
    let mut key_all = Input::new(key2, InputConfig::default().with_pull(Pull::Up));
    loop {
        key_all.wait_for_falling_edge().await;
        let num = judge_adc_num().await;      // 0 = 抖动，忽略
        if num == 0 { Timer::after_millis(10).await; continue; }
        key_detection(&mut key_all, num).await;
        refresh_active_time().await;
        crate::sleep::mark_user_touch();
        Timer::after_millis(10).await;
    }
}
```

v1 是双路：GPIO9 直连键（键1）与 GPIO2 梯（键2/3）用 `embassy_futures::select` 同时等两路下降沿：

```rust
// src/event.rs（v1，有删减）
pub async fn run(key1: GPIO9<'static>, key2: KeyLadderGpio) {
    loop {
        match select(key1.wait_for_falling_edge(), key2.wait_for_falling_edge()).await {
            Either::First(_) => key_detection(&mut key1, 1).await,
            Either::Second(_) => { /* judge_adc_num() → key_detection(&mut key2, num) */ }
        }
    }
}
```

`key_detection` 是共用的按键状态机：100 次采样里 >80 次为低判定"按下"，持续 >500ms 出 `KeyLongStart`、之后每 100ms 出一个 `KeyLongIng`，释放出 `KeyLongEnd`；短按则再等 400ms 看有没有第二按下，有则出 `KeyDouble`（受 `ENABLE_DOUBLE` 开关控制），无则出 `KeyShort`。

### 6.3 事件队列：扫描与派发解耦

早期实现里 `toggle_event` 直接在扫描路径上遍历监听回调——一个慢回调会阻塞扫描，期间的按键就丢了。现在拆成生产者-消费者：

```rust
// src/event.rs
/// 按键事件队列：容量 8。生产者=toggle_event（扫描/状态机），消费者=dispatch 任务。
static EVENT_QUEUE: Mutex<CriticalSectionRawMutex, Vec<EventType, 8>> = Mutex::new(Vec::new());
static EVENT_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
```

- **生产者** `toggle_event` 只入队：`KeyLongIng` 连发在队尾同键合并（不堆积），队列满丢最旧保最新，然后 `EVENT_READY.signal()`
- **消费者** `dispatch` 任务被信号唤醒后 FIFO 串行派发，逐个调用匹配的监听回调

代价是一条约束：派发期间持有 `LISTENER` 锁且回调串行执行——**回调内禁止调用 `on` / `clear` / `on_fixed` 重绑定事件**（会死锁）；页面切换的重绑定走页面 `run` 循环的既有约定。

## 7. RTC：实时时钟与睡眠控制

```rust
// src/main.rs:135-136
let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
crate::sleep::RTC_MANGE.lock().await.replace(rtc);
```

RTC（Real-Time Clock）是 ESP32-C3 的低功耗外设，它独立于 CPU 运行，即使在深度睡眠期间也能保持计时。本项目中，ESP RTC 的作用包括：
- 记录进入睡眠的时间，唤醒后通过差值恢复系统时间（rtc_fast 估算路径）
- 配置唤醒源（定时器、GPIO——按键梯脚以 RTC IO 低电平唤醒注册）
- 控制深度睡眠

`LPWR`（Low Power）外设在 esp-hal v1.x 中直接从 `Peripherals` 获取，取代了旧版的 `RTC_CNTL`。

v2 新板还有第二颗时钟：外置 PCF8563 硬件 RTC（见第 4 节）。分工是——**系统时间恢复优先 PCF8563**（NTP 对时后写入，CR2032 备电维持），rtc_fast 估算退居回退路径；**定时唤醒优先 PCF8563 的 INT 脚**（走时精确），ESP RTC 定时器作为布防失败时的回退。旧板没有 PCF8563，两者都回落到 ESP RTC。

## 小结

本文深入了本项目的硬件抽象设计：

| 技术点 | 要点 |
|--------|------|
| 外设所有权 | `Peripherals` 的每个字段只能被移动一次，例外走 `Peripherals::steal()` |
| GPIO 新 API | `Output::new(pin, level, OutputConfig)` 增加配置结构体 |
| 板级抽象 | `epd_*` × `board_*` 两维 feature，board 文件提供统一 API，电源轨/I2C/按键按板切换 |
| SPI 配置 | `SpiConfig` Builder 模式，`Rate::from_mhz()` 频率设置 |
| SPI 共享 | `CsMutex` + `CriticalSectionDevice`，临界区互斥 |
| I2C 共享 | `i2c_bus` 静态 Mutex 封装，挂 PCF8563(0x51) / AHT20(0x38) |
| 全局状态 | 四种模式：Embassy Mutex / core::ptr / StaticCell / RTC 内存 |
| ADC | 严格泛型参数 + 板级类型别名，12 次采样取中位数，板型阈值区分键号 |
| 事件队列 | `toggle_event` 入队 + `dispatch` 串行派发，长回调期间按键不丢 |
| RTC | `LPWR` 外设控制深睡；v2 外置 PCF8563 管时钟恢复与定时唤醒 |

核心思想是：**用 Rust 的类型系统在编译期保证硬件访问的安全性（守卫、所有权、类型别名），用运行时锁在必要时协调并发访问（临界区、异步 Mutex）**。

在下一篇文章中，我们将进入电子墨水屏的世界，看看显示渲染架构是如何设计的。

---

> 上一篇：[第 1 篇：项目全景](01-项目全景.md) · 下一篇：[第 3 篇：电子墨水屏驱动与渲染架构](03-电子墨水屏驱动与渲染架构.md)
