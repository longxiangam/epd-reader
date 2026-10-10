# 第 5 篇：WiFi 网络与 HTTP 通信

> English: [en/05-wifi-and-http.md](en/05-wifi-and-http.md)

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

## 引言

ESP32-C3 只有约 400KB SRAM，却要跑完整的 WiFi + TCP/IP + TLS + HTTP。本文讲这套网络栈在本项目里是怎么搭起来的：WiFi 驱动、HTTP/HTTPS 请求客户端，以及 AP 模式下自建 DHCP/DNS 的配网方案。

## 1. WiFi 驱动层：从 esp-wifi 到 esp-radio

### 1.1 初始化

本项目使用 `esp-radio` 0.18——这是 esp-rs 生态中将 WiFi 驱动从 `esp-hal` 中独立出来的新 crate。相比旧版 `esp-wifi` 0.7，API 变化很大但更简洁：

```rust
// src/wifi.rs:88-168
pub async fn connect_wifi(
    spawner: &Spawner,
    rng: Rng,
    wifi: esp_hal::peripherals::WIFI<'static>,
) -> Result<&'static Stack<'static>, WifiNetError> {
    // connect_wifi 由 main 在最后一步调用；若前面已有任务抢先 use_wifi，
    // 会先等到 REINIT_WIFI_SIGNAL 信号才开始真正的初始化
    REINIT_WIFI_SIGNAL.wait().await;
    HAL_RNG.lock().await.replace(rng);   // RNG 交给全局，TLS 握手要用

    // 从 Flash 读取 WiFi 配置
    let ssid = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_ssid.clone();
    let password = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_password.clone();

    // 配置 Station 模式
    let station_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(ssid.as_str())
            .with_password(password.as_str().into()),
    );

    // 一行创建 WiFi 控制器和网络接口
    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    ).unwrap();

    let wifi_interface = interfaces.station;
    // ...
}
```

对比旧版需要的步骤：
- ~~`esp_wifi::initialize(EspWifiInitFor::Wifi, timer, rng, radio_clk, &clocks)`~~
- ~~`esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)`~~

新版只需要 `esp_radio::wifi::new(wifi, config)` 一行，不需要手动传入定时器、RNG、无线电时钟等参数。WiFi 外设从 `Peripherals` 获取，所有权通过函数参数转移。

### 1.2 网络栈：Stack 与 Runner 分离

embassy-net 0.9 最大的架构变化是将网络栈分为两部分：

```rust
// src/wifi.rs:119-130
let (stack, runner) = embassy_net::new(
    wifi_interface,
    Config::dhcpv4(Default::default()),  // DHCP 自动获取 IP
    make_static!(StackResources::<4>::new()),  // 最多 4 个并发连接
    seed,  // 随机种子（硬件 RNG 生成）
);
let stack: &Stack<'static> = &*make_static!(stack);

spawner.spawn(net_task(runner).unwrap());   // Runner 作为独立任务
spawner.spawn(connection_wifi(controller).unwrap());
spawner.spawn(do_stop().unwrap());          // 30 秒闲置自动关 WiFi
```

**Stack** 是网络栈的接口，用于创建 Socket、查询 DNS、获取 IP 等。
**Runner** 是网络栈的引擎，在独立任务中驱动 TCP/IP 协议栈处理。

这种分离的好处是：网络栈的运行不占用调用者的执行时间。调用者只需通过 Stack 接口进行操作，协议栈的处理在后台异步完成。

Runner 任务非常简单：

```rust
// src/wifi.rs:171-174
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Interface<'static>>) {
    runner.run().await
}
```

### 1.3 等待连接就绪（带超时）

```rust
// src/wifi.rs:133-164（节选）
let init_start_secs = Instant::now().as_secs();
// 等待物理链路就绪，最多 INIT_WAIT_TIMEOUT_SECS = 35 秒
loop {
    if stack.is_link_up() { break; }
    if matches!(*WIFI_STATE.lock().await, Some(WifiNetState::WifiStopped)) {
        return Err(WifiNetError::TimeOut);   // 等待期间被停掉
    }
    if Instant::now().as_secs() - init_start_secs > INIT_WAIT_TIMEOUT_SECS {
        return Err(WifiNetError::TimeOut);
    }
    Timer::after(Duration::from_millis(1000)).await;
}

// 等待 DHCP 分配 IP，再给 10 秒宽限
loop {
    if let Some(config) = stack.config_v4() {
        println!("Got IP: {}", config.address);
        break;
    }
    if Instant::now().as_secs() - init_start_secs > INIT_WAIT_TIMEOUT_SECS + 10 {
        return Err(WifiNetError::TimeOut);
    }
    Timer::after(Duration::from_millis(500)).await;
}
```

WiFi 连接分两个阶段：先是物理层关联（link up），然后是 DHCP 获取 IP 地址。两个阶段都通过轮询 + `Timer::after` 实现——在协作式调度下，轮询不会阻塞其他任务。

与早期"无限等下去"不同，现在两阶段都有基于时间的上限：拿到 IP 前 `STACK_MUT` 不会写入，`use_wifi` 也就不会放行任何请求——开机连不上网时，主流程能立刻拿到 `Err` 去走离线路径，而不是卡死在启动页。

## 2. WiFi 生命周期管理

WiFi 是耗电大户，ESP32-C3 在 WiFi 开启时功耗约 80-100mA，关闭后可降至微安级。因此项目实现了精细的 WiFi 生命周期管理。

### 2.1 状态机

```rust
// src/wifi.rs:33-39
pub enum WifiNetState {
    WifiConnecting,   // 正在连接
    WifiConnected,    // 已连接
    WifiDisconnected, // 断开
    WifiStopped,      // 已停止（省电模式）
}
```

状态转换：

```
                     ┌──────────────┐
                     │  WifiStopped │ ← 30秒无网络请求 / 连续5次连接失败
                     └──────┬───────┘
                            │ RECONNECT_WIFI_SIGNAL
                            ▼
  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
  │WifiDisconnected│─►│WifiConnecting │─►│WifiConnected │
  └──────────────┘   └──────────────┘   └──────┬───────┘
         ▲                                       │
         │ 连接断开                               │ STOP_WIFI_SIGNAL
         └───────────────────────────────────────┘
```

### 2.2 连接管理任务

`connection_wifi` 任务负责维护 WiFi 连接：

```rust
// src/wifi.rs:176-266（核心逻辑简化）
#[embassy_executor::task]
async fn connection_wifi(mut controller: WifiController<'static>) {
    let mut consecutive_failures: u32 = 0;
    loop {
        if controller.is_connected() {
            consecutive_failures = 0;
            WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);

            // 等待断开事件或停止信号
            loop {
                let mut subscriber = controller.subscribe().unwrap();
                let close_signal = STOP_WIFI_SIGNAL.wait();
                match select(subscriber.next_event_pure(), close_signal).await {
                    First(_) => {
                        // WiFi 事件（可能是断开）
                        if !controller.is_connected() {
                            WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                            break;   // 退出内层循环，走下面的重连分支
                        }
                    }
                    Second(_) => {
                        // 收到停止信号
                        STOP_WIFI_SIGNAL.reset();
                        let _ = controller.disconnect_async().await;
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiStopped);
                        RECONNECT_WIFI_SIGNAL.wait().await;  // 睡到重新连接信号
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                        break;
                    }
                }
            }
        }

        // 每次连接前都从 Flash 重新读 SSID/密码（支持不重启换网）
        // ... controller.set_config(&station_config) ...

        WIFI_STATE.lock().await.replace(WifiNetState::WifiConnecting);
        match controller.connect_async().await {
            Ok(_) => {
                consecutive_failures = 0;
                WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONNECT_RETRIES {
                    // 连续 5 次失败：放弃并置 WifiStopped（等于主动关 WiFi），
                    // 等 RECONNECT_WIFI_SIGNAL 再试——避免密码错误时永远空转耗电
                    let _ = controller.disconnect_async().await;
                    WIFI_STATE.lock().await.replace(WifiNetState::WifiStopped);
                    RECONNECT_WIFI_SIGNAL.wait().await;
                } else {
                    Timer::after(Duration::from_millis(CONNECT_RETRY_INTERVAL_MS)).await; // 5 秒后重试
                }
            }
        }
    }
}
```

esp-radio 0.18 的事件监听 API 使用 `subscribe() + next_event_pure()` 模式，取代了旧版的 `wait_for_event()`。这种方式可以获取所有 WiFi 事件，而不只是特定的一种。

### 2.3 自动关闭机制

```rust
// src/wifi.rs:371-385
#[embassy_executor::task]
async fn do_stop() {
    loop {
        if let Some(WifiNetState::WifiConnected) = *WIFI_STATE.lock().await {
            // 30 秒没有网络请求，且当前没有请求持锁，才自动关闭 WiFi
            if Instant::now().as_secs() - LAST_USE_TIME_SECS.lock().await.unwrap() > HOW_LONG_SECS_CLOSE
                && !*WIFI_LOCK.lock().await {
                STOP_WIFI_SIGNAL.signal(());
            }
        }
        Timer::after(Duration::from_millis(3000)).await  // 每 3 秒检查一次
    }
}
```

### 2.4 网络锁：use_wifi / finish_wifi

网络请求前需要"锁定"WiFi，防止在使用过程中被关闭：

```rust
// src/wifi.rs:278-359（简化）
const TIME_OUT_SECS: u64 = 10;
/// 阅读期间置 true：拦截所有 use_wifi 请求（阅读/联网运行期互斥）。
pub static WIFI_BLOCKED: AtomicBool = AtomicBool::new(false);

pub async fn use_wifi() -> Result<&'static Stack<'static>, WifiNetError> {
    // 阅读期间禁止联网（阅读独占内存，WiFi 已停）
    if WIFI_BLOCKED.load(Ordering::Acquire) {
        return Err(WifiNetError::Using);
    }
    let secs = Instant::now().as_secs();
    // 等待获取锁，最多 10 秒
    loop {
        if !*WIFI_LOCK.lock().await { break; }
        if Instant::now().as_secs() - secs > TIME_OUT_SECS {
            return Err(WifiNetError::TimeOut);
        }
        // 等锁期间刻意不刷新"网络使用时间"：锁持有者自己会刷。
        // 否则睡前 force_stop_wifi 会误判"仍有请求"而永远等下去。
        Timer::after(Duration::from_millis(500)).await;
    }
    *WIFI_LOCK.lock().await = true;  // 加锁

    let result = use_wifi_locked(secs).await;
    if result.is_err() {
        *WIFI_LOCK.lock().await = false;  // 失败路径也释放锁
        println!("use_wifi failed, wifi lock released");
    }
    result
}

async fn use_wifi_locked(secs: u64) -> Result<&'static Stack<'static>, WifiNetError> {
    if *WIFI_STATE.lock().await == None {
        REINIT_WIFI_SIGNAL.signal(());   // 触发初始化（3 秒内没动静 → WaitConnecting）
    }
    if WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
        RECONNECT_WIFI_SIGNAL.signal(()); // 触发重连
    }

    const WAIT_STACK_SECS: u64 = 30;
    loop {
        refresh_last_time().await;        // 等连接期间持续刷新使用时间
        unsafe {
            let stack = *core::ptr::addr_of!(STACK_MUT);
            if let Some(v) = stack {
                if v.is_link_up() {
                    v.wait_config_up().await;
                    return Ok(v);
                } else if 超过 TIME_OUT_SECS {
                    return Err(WifiNetError::TimeOut);   // 链路 10 秒起不来
                }
            } else if 超过 WAIT_STACK_SECS {
                // 冷启动/唤醒后 DHCP 可能要十几秒，STACK_MUT 迟迟没写入 → 放弃
                return Err(WifiNetError::Infallible);
            }
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

pub async fn finish_wifi() {
    refresh_last_time().await;  // 刷新最后使用时间
    *WIFI_LOCK.lock().await = false;  // 解锁
}
```

使用模式：

```rust
// 典型用法
let stack = use_wifi().await?;           // 获取网络栈（确保WiFi已连接）
let client = RequestClient::new(stack).await;
let response = client.send_request(url).await?;
finish_wifi().await;                      // 释放网络栈（开始30秒倒计时）
```

`WIFI_LOCK` 是所有网络请求的串行化点——任意时刻只有一个请求在途，这正是 `request.rs` 里那些 `static mut` 缓冲可以安全取可变引用的前提（单核 + 锁）。几个细节值得注意：

- **锁的失败路径必须释放**：`use_wifi` 拿到锁之后的任何失败（初始化超时、链路超时、stack 未就绪）都会把锁释放掉，否则后续所有请求都会永久死锁，睡前关 WiFi 也会被卡住；
- 入睡前的 `force_stop_wifi()` **绝不抢占**在途请求：它会先等 `WIFI_LOCK` 释放（单请求有界：链路等待 ~10s + socket 超时 10s，合计 ~30s 内必释放；45s 兜底），再发停止信号，避免强杀正在读写 socket 的请求。`do_stop` 的 30 秒闲置判断也同时要求锁未被持有，因为 socket 长时间 `read` 期间不会刷新使用时间；
- 进阅读页时有 `force_stop_wifi_quick()`：最多只等 3 秒在飞请求就强制停 WiFi（阅读独占内存不能久等），停不下来也直接返回；
- `refresh_last_time()` 只刷"网络使用时间"，不复位用户活动时间——否则后台 NTP 重试会不停续命，导致设备永远无法判断空闲入睡。

## 3. HTTP/HTTPS 客户端

### 3.1 请求客户端

`RequestClient` 封装了 HTTP/HTTPS **GET** 请求：

```rust
// src/request.rs
pub struct RequestClient {
    stack: &'static Stack<'static>,
    rng: RngWrapper,   // 取自 wifi::HAL_RNG，作为 TLS 握手的随机源
}
```

真正占内存的是各收发缓冲，它们被声明为 `static mut` 放进 `.bss`（不占堆），由 `WIFI_LOCK` 串行化保证独占使用（ESP32-C3 单核 + 任意时刻只有一个请求在途）：

| 缓冲 | 大小 | 用途 |
|------|------|------|
| `RX_BUF / TX_BUF` | 各 4 KB | TCP 收发 |
| `TLS_RX_BUF / TLS_TX_BUF` | 各 8 KB | TLS 收发（握手记录 + 加密流量） |
| `HEADERS_BUF_` | 1 KB | 响应头 |
| `RESPONSE_BUF_` | 8 KB | 响应体（新浪日 K 60 根约 6.4KB、分时约 5.8KB；天气/节假日 < 4KB） |

合计约 33KB 静态缓冲。把它们移出堆、放进 `.bss`，堆才能缩进独立的 dram2 段（~64KB， bootloader 释放的 DRAM），主 DRAM 全部让给调用栈。

### 3.2 三种发送方式与自定义 header

```rust
// 拷贝版：把响应体拷进 Vec 返回（适合响应小、不常请求的调用方，如 IP 定位、天气）
pub async fn send_request(url: &str) -> Result<ResponseData, RequestError>

// 零拷贝版：返回 &'static [u8]，指向 RESPONSE_BUF_；契约——在下一次 send_request* 前有效
pub async fn send_request_slice(url: &str) -> Result<&'static [u8], RequestError>

// 带自定义 header 的零拷贝版（如新浪行情接口需要 Referer）
pub async fn send_request_slice_with(url: &str, headers: &[(&str, &str)])
    -> Result<&'static [u8], RequestError>
```

URL 在内部解析：`https://` 默认端口 443、`http://` 默认 80，其它 scheme 返回 `UnsupportedScheme`；path 为空时补 `/`。错误类型 `RequestError` 覆盖 DNS / 连接 / TLS / 超时 / 缓冲溢出等，股票页会把它映射成中文短串显示。

调用方在请求前后还会置位 `wifi::set_request_loading(true/false)`——天气、日历、股票页据此在屏幕上画出"加载中"角标，让用户知道设备正在联网。

### 3.3 HTTPS 请求流程

HTTPS 在 TCP 之上加 TLS 层（简化）：

```rust
// 1. DNS 解析
let ip_address = resolve(host).await?;
// 2. TCP 连接（10s 超时）
let mut socket = TcpSocket::new(*stack, &mut RX_BUF, &mut TX_BUF);
socket.set_timeout(Some(Duration::from_secs(10)));
socket.connect((ip_address, port)).await?;
// 3. TLS 握手
let config = TlsConfig::new().with_server_name(host).enable_rsa_signatures();
let mut tls = TlsConnection::new(socket, &mut TLS_RX_BUF, &mut TLS_TX_BUF);
tls.open(TlsContext::new(&config,
    UnsecureProvider::new::<Aes128GcmSha256>(&mut rng))).await?;
// 4. GET 请求 + 读响应到 RESPONSE_BUF_
let request = Request::get(path).host(host).headers(headers).build();
request.write_header(&mut tls).await?;
let response = Response::read(&mut tls, Method::GET, &mut HEADERS_BUF_).await?;
response.body().reader().read_to_end(&mut RESPONSE_BUF_).await?;
```

注意 TLS 用 `UnsecureProvider`——**不做证书验证**。完整的 CA 证书链需要几百 KB 存根证书，ESP32-C3 没这么多富余内存；对天气/股票这类非敏感数据是可接受的折中。加密套件固定 `Aes128GcmSha256` 并启用 RSA 签名。所有请求都是 GET（无 POST/PUT）。

明文 HTTP 与 HTTPS 走同一条路，只是少了第 3 步（TLS 握手）且复用同一组 `RX_BUF/TX_BUF`。

### 3.4 基于 IP 的自动定位

`location.rs` 用一个 IP 归属接口根据出口 IP 反查城市，作为天气"自动定位"的数据源：

```rust
// src/location.rs
// 国内 CDN、UTF-8、免 key；响应形如：
// {"ret":"ok","data":{"ip":"...","location":["中国","湖北","武汉","","移动"]}}}
const LOCATE_URL: &str = "https://myip.ipip.net/json";

pub struct LocateResult {
    pub city: String<32>,    // 城市名（location 数组第 3 个字段，如"武汉"）
    pub latlon: String<32>,  // 位置参数（现与城市名相同，心知天气接受城市名）
}

pub async fn locate() -> Option<LocateResult> {
    // 标准 use_wifi → send_request(拷贝版) → finish_wifi 三段式
    // 手工查找 "location" 数组并依次读出带引号的字段（不引 JSON 解析器）
    // 城市字段为空 → 返回 None
}
```

这个接口经历过一次换源：旧版用 `ip-api.com` 的明文 HTTP + 经纬度（`"lat:lon"` 格式），但它是境外服务，国内直连 TCP 常超时，已弃用；现在的 `myip.ipip.net` 是境内服务，走 HTTPS，返回的城市名可以直接喂给心知天气的 `location` 参数（经纬度之外它也支持城市名查询）。设置页的"自动定位"项调用它，成功后把城市写入 `WeatherStorage.city`（跨重启保留，菜单条目状态也从这里读）并立刻拉一次天气。

顺带一提：若编译时启用 `weather-openmeteo` feature，天气走 Open-Meteo 接口，此时 `city` 字段也可手动填成 `"纬度:经度"`（或 `"纬度,经度"`) 坐标串，两种格式都能解析。

### 3.5 天气与节假日数据源

天气和节假日是开机后仅有的两类周期性 HTTP 请求，都由后台的 `ntp_worker` 触发：时间同步过至少一次（`sync_time_success()`）后，worker 每轮循环（对时间隔 3600 秒）末尾都会调用同步入口，由同步入口内部再用"数据是否过期"决定要不要真的发请求：

- **天气**（`weather.rs`）：默认用心知天气 `api.seniverse.com` 的 5 天预报（token 可在配网页配置，代码里内置了一个默认 key）；启用 `weather-openmeteo` feature 则改走 `api.open-meteo.com`（HTTPS + 坐标查询）。成功后连同同步时间戳一起写进 `WeatherStorage`（Flash），下次唤醒直接从 Flash 读缓存；
- **过期判断**：数据超过 5 小时算过期（`weather_stale()` / `stale_in_secs()`）。定时唤醒醒来时先查这个——数据还新鲜就连网都不用，读完本地缓存重绘立即回睡；
- **节假日**（`weather.rs` 中 `HolidayInfo`）：数据源是 GitHub 上 `NateScarlet/holiday-cn` 的年度 JSON。境内直连 `raw.githubusercontent.com` 会被 SNI 阻断（TLS 握手即被 RST，表现为 `TlsError(Io(ConnectionReset))`），所以按 **三级 URL 回退**依次尝试：

```rust
// src/weather.rs:224-228
let urls = [
    format!("https://cdn.jsdelivr.net/gh/NateScarlet/holiday-cn@master/{}.json", current_year),
    format!("https://fastly.jsdelivr.net/gh/NateScarlet/holiday-cn@master/{}.json", current_year),
    format!("https://raw.githubusercontent.com/NateScarlet/holiday-cn/master/{}.json", current_year),
];
// 逐个尝试，任一成功即止；全部失败打印最后一个错误
```

前两个是同一份数据的 jsDelivr CDN 镜像（境内可达），最后回落 GitHub 源站（境外设备仍可用）。请求全程包裹在 `set_request_loading(true/false)` 里；解析失败（可能是 429 限流）会打印响应原文辅助排查；成功后把更新时间写进 `SyncStampStorage` 供设置页展示。

## 4. AP 模式与配网

首次使用时，设备没有 WiFi 配置（`wifi_finish == false`）。此时进入 AP（Access Point）模式，创建一个热点"esp_wifi"，手机连接后通过 Web 页面配置 WiFi 信息。

### 4.1 AP 模式初始化

```rust
// src/wifi.rs:428-498（简化）
pub async fn start_wifi_ap(spawner: &Spawner, rng: Rng, wifi: ...) {
    let ap_config = WifiConfig::AccessPoint(
        AccessPointConfig::default().with_ssid("esp_wifi")
    );

    let (controller, interfaces) = esp_radio::wifi::new(wifi, config)?;
    let wifi_ap_interface = interfaces.access_point;

    // AP 模式使用静态 IP（不从 DHCP 获取）
    let ap_net_config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 2, 1), 24),
        gateway: Some(Ipv4Address::new(192, 168, 2, 1)),
        dns_servers: Default::default(),
    });

    let (ap_stack, runner) = embassy_net::new(wifi_ap_interface, ap_net_config, ...);
    spawner.spawn(ap_task(runner)).unwrap();            // 网络栈 Runner
    spawner.spawn(dhcp_service()).unwrap();  // 自建 DHCP 服务
    spawner.spawn(dns_service()).unwrap();   // 自建 DNS 服务
    spawner.spawn(connection_wifi_ap(controller)).unwrap();
}
```

AP 栈单独存放在 `AP_STACK_MUT`，与 STA 模式的 `STACK_MUT` 互不干扰。哪种模式由 `WIFI_MODEL`（`WifiModel::AP / STA`）决定：配网启动走 AP，正常启动走 STA。

### 4.2 自建 DHCP 服务

AP 模式下没有现成的 DHCP 服务器。本项目用 UDP socket 在 67 端口手动实现了 DHCP 协议的核心部分：

```rust
// src/wifi.rs:534-596（简化）
#[embassy_executor::task]
async fn dhcp_service() {
    let mut udp_socket = UdpSocket::new(*ap_stack, ...);
    udp_socket.bind(67);  // DHCP 服务端口

    loop {
        match udp_socket.recv_from(&mut buf).await {
            Ok((n, src)) => {
                let msg = Message::new(buf).unwrap();
                let options = v4_options!(msg; MessageType required, ...);
                match options {
                    Ok((msg_type, _, _)) => {
                        if msg_type == MessageType::DISCOVER {
                            send_dhcp_offer(&udp_socket, src, &msg).await;
                        } else if msg_type == MessageType::REQUEST {
                            send_dhcp_ack(&udp_socket, src, &msg).await;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
```

DHCP 分配策略：
- 分配 IP：`192.168.2.2`
- 网关：`192.168.2.1`（ESP32 自己）
- DNS：`192.168.2.1`（ESP32 自己）
- 租约时间：3600 秒

使用 `dhcparse` crate 解析和构建 DHCP 报文，避免手动处理二进制协议。

### 4.3 DNS 劫持服务

为了让用户无论输入什么域名都能访问到配网页面，DNS 服务会劫持所有查询：

```rust
// src/wifi.rs:675-746（简化）
#[embassy_executor::task]
async fn dns_service() {
    let mut udp_socket = UdpSocket::new(*ap_stack, ...);
    udp_socket.bind(53);  // DNS 服务端口

    loop {
        match udp_socket.recv_from(&mut buf).await {
            Ok((n, src)) => {
                // 无论查询什么域名，都返回 ESP32 自己的 IP
                let response = create_dns_response(LOCAL_IP, &buf[..n]);
                udp_socket.send_to(&response, src).await;
            }
            _ => {}
        }
    }
}

fn create_dns_response(ip: Ipv4Addr, request: &[u8]) -> Vec<u8, 512> {
    let mut response = Vec::new();
    response.extend_from_slice(&request[0..2]).unwrap();  // 事务 ID
    response.extend_from_slice(&[0x81, 0x80]).unwrap();   // 标志：标准响应
    // ... 复制查询，添加回答（TTL 60s，A 记录）...
    response.extend_from_slice(&ip.octets()).unwrap();    // ESP32 的 IP
    response
}
```

这意味着用户在手机浏览器中输入任何网址（如 `www.baidu.com`），都会被解析到 ESP32 的 IP `192.168.2.1`，从而打开配网页面。这是一种常见的 Captive Portal 技术。

## 5. Web 配置服务

配网服务运行在 AP 模式下的 TCP 80 端口，提供 Web 页面和 REST API。它同样可以在 STA 模式下运行（设置页的二维码界面会按需启停，用 `STOP_WEB_SERVICE` 信号通知 accept 循环退出）：

```rust
// src/web_service.rs:44-134（简化）
async fn web_tcp_socket(stack: &'static Stack<'static>) {
    let mut socket = TcpSocket::new(*stack, &mut rx_buffer, &mut tx_buffer);  // 各 1536B
    loop {
        // accept 与停止信号 select：信号到来时优雅退出
        select(STOP_WEB_SERVICE.wait(), socket.accept(...)).await;

        // Keep-alive：同一 TCP 连接处理多个 HTTP 请求
        loop {
            // 1) 循环 read 直到出现 \r\n\r\n（header 结束）
            // 2) 解析 Content-Length，继续把 body 读完再交给 process_http
            process_http(&mut socket, &buffer[..pos]).await;
        }
        socket.close();
    }
}
```

支持的 API：

| 路径 | 方法 | 功能 |
|------|------|------|
| `/`、`/index.html` | GET | 返回配网 HTML 页面（`files/config.html`，`Cache-Control: no-store`） |
| `/config` | GET | 返回全部配置 JSON（wifi / weather / sleep / display / stocks / reading / home_page） |
| `/configure_wifi` | POST | 保存 WiFi 配置；SSID 或密码**有变化才** `software_reset()` 重启 |
| `/configure_weather` | POST | 保存天气 token / 城市 |
| `/configure_sleep` | POST | 保存阅读 / 日历睡眠时长 |
| `/configure_display` | POST | 保存全刷间隔（钳制 1..=100，越界回退平台默认） |
| `/configure_stock` | POST | 保存最多 6 对（代码, 名称）股票（`code0..code5` / `name0..name5`） |
| `/configure_reading` | POST | 保存阅读排版：字号 px（12..=40）、行距 gap（-10..=10）、旋转 rotate（0..3）、字体文件名 |
| `/books` | GET | 列出 SD 卡 `books` 目录中的电子书（流式 JSON，逐个写书名） |
| `/images` | GET | 列出 SD 卡 `images` 目录中的图片 |
| `/upload?name=XX&chunk=N` | POST | 分块上传电子书（URL 编码的中文长文件名，`url_decode` 后走 LFN 创建） |
| `/upload_image?name=XX&chunk=N` | POST | 分块上传图片 |
| `/delete` | POST | 删除电子书（连同旧 `.idx` 与 `.log` 进度文件） |
| `/delete_image` | POST | 删除图片 |
| `/sleep_image` | GET | 查询待机壁纸状态（`flash_sleep` 分区，返回大小） |
| `/upload_sleep_image` | POST | 上传待机壁纸（限 30KB，写入 Flash） |
| `/delete_sleep_image` | POST | 删除待机壁纸 |

所有 JSON 响应都带 `Content-Length` 和 `Connection: keep-alive` 头。表单解析（`parse_form`，multipart/form-data）支持最多 20 个字段，并按 `Content-Length` 跨 TCP 分段把 body 读完——这是为多支股票大表单做的修复，避免分段时丢字段。文件上传支持分块（chunk），解决了嵌入式设备 TCP 缓冲区有限的问题：`chunk=0` 用 `ReadWriteCreateOrTruncate` 新建，后续块用 `ReadWriteCreateOrAppend` 追加，每块通过 URL 参数 `chunk=N` 指定序号；已随 header 读进来的 body 部分会先落盘再继续读 socket，不丢字节。

上传/删除的关键是**中文长文件名**：书名先 `url_decode`，再经 fork 版 `embedded-sdmmc` 的 `open_long_name_file_in_dir` 直接按长名建文件（详见第 6 篇）。

## 小结

| 技术点 | 要点 |
|--------|------|
| WiFi 驱动 | `esp-radio` 0.18，`wifi::new()` 一行初始化 |
| 网络栈 | embassy-net 0.9，Stack/Runner 分离 |
| WiFi 生命周期 | 4 种状态 + Signal 驱动的状态转换；初始化/连接全程有超时 |
| 连接重试 | 连续 5 次失败放弃转 `WifiStopped`，等信号再试 |
| 自动省电 | 30 秒无请求自动关闭 WiFi（有请求持锁时不关） |
| HTTP | reqwless 0.14，GET 请求，~33KB 静态 `.bss` 缓冲 |
| HTTPS | embedded-tls 0.18，AES-128-GCM，跳过证书验证 |
| IP 定位 | myip.ipip.net（HTTPS，境内可达，返回城市名） |
| 节假日源 | jsDelivr CDN ×2 → GitHub 源站，三级 URL 回退 |
| AP 配网 | 自建 DHCP + DNS 劫持（Captive Portal） |
| Web 服务 | TCP 80 端口，Keep-alive，分块上传，AP/STA 双模式 |

在只有 400KB SRAM 的 ESP32-C3 上实现完整的 WiFi + TCP/IP + TLS + HTTP 协议栈，Embassy 的异步架构功不可没——它让网络操作不会阻塞 UI，而精细的 WiFi 生命周期管理确保了电池续航。

在下一篇文章中，我们将进入存储层，看看 Flash 持久化和 SD 卡文件系统是如何实现的。

---

> 上一篇：[第 4 篇：Embassy 异步运行时与事件驱动架构](04-Embassy异步运行时与事件驱动.md) · 下一篇：[第 6 篇：数据持久化与文件系统](06-数据持久化与文件系统.md)
