# 第 5 篇：WiFi 网络与 HTTP 通信

> 用 Rust 构建ESP32-C3 电子墨水屏阅读器 · 系列文章

## 引言

让一个 400KB SRAM 的微控制器连接 WiFi、跑 TCP/IP 协议栈、发起 HTTPS 请求——这在传统嵌入式开发中几乎不可想象。但在 Embassy + esp-radio 的加持下，Rust 让这一切变得优雅而可靠。

本文将覆盖从 WiFi 驱动到 HTTP 客户端的完整网络栈，以及一个在 AP 模式下自建 DHCP/DNS 服务的配网方案。

## 1. WiFi 驱动层：从 esp-wifi 到 esp-radio

### 1.1 初始化

本项目使用 `esp-radio` 0.18——这是 esp-rs 生态中将 WiFi 驱动从 `esp-hal` 中独立出来的新 crate。相比旧版 `esp-wifi` 0.7，API 变化很大但更简洁：

```rust
// src/wifi.rs:85-153
pub async fn connect_wifi(
    spawner: &Spawner,
    rng: Rng,
    wifi: esp_hal::peripherals::WIFI<'static>,
) -> Result<&'static Stack<'static>, WifiNetError> {
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
// src/wifi.rs:116-122
let (stack, runner) = embassy_net::new(
    wifi_interface,
    Config::dhcpv4(Default::default()),  // DHCP 自动获取 IP
    make_static!(StackResources::<4>::new()),  // 最多 4 个并发连接
    seed,  // 随机种子
);
let stack: &Stack<'static> = &*make_static!(stack);

spawner.spawn(net_task(runner)).unwrap();  // Runner 作为独立任务
```

**Stack** 是网络栈的接口，用于创建 Socket、查询 DNS、获取 IP 等。
**Runner** 是网络栈的引擎，在独立任务中驱动 TCP/IP 协议栈处理。

这种分离的好处是：网络栈的运行不占用调用者的执行时间。调用者只需通过 Stack 接口进行操作，协议栈的处理在后台异步完成。

Runner 任务非常简单：

```rust
// src/wifi.rs:155-158
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Interface<'static>>) {
    runner.run().await
}
```

### 1.3 等待连接就绪

```rust
// src/wifi.rs:130-148
// 等待物理链路就绪
loop {
    if stack.is_link_up() { break; }
    Timer::after(Duration::from_millis(1000)).await;
}

// 等待 DHCP 分配 IP
loop {
    if let Some(config) = stack.config_v4() {
        println!("Got IP: {}", config.address);
        break;
    }
    Timer::after(Duration::from_millis(500)).await;
}
```

WiFi 连接分两个阶段：先是物理层关联（link up），然后是 DHCP 获取 IP 地址。两个阶段都通过轮询 + `Timer::after` 实现——在协作式调度下，轮询不会阻塞其他任务。

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
                     │  WifiStopped │ ← 30秒无网络请求
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
// src/wifi.rs:160-232（核心逻辑简化）
#[embassy_executor::task]
async fn connection_wifi(mut controller: WifiController<'static>) {
    loop {
        if controller.is_connected() {
            // 已连接，等待断开或停止信号
            loop {
                let mut subscriber = controller.subscribe().unwrap();
                let close_signal = STOP_WIFI_SIGNAL.wait();
                match select(subscriber.next_event_pure(), close_signal).await {
                    First(_) => {
                        // WiFi 事件（可能是断开）
                        if !controller.is_connected() {
                            // 断开了，退出内层循环，重新连接
                            break;
                        }
                    }
                    Second(_) => {
                        // 收到停止信号
                        controller.disconnect_async().await;
                        RECONNECT_WIFI_SIGNAL.wait().await;  // 等待重新连接信号
                        break;
                    }
                }
            }
        }
        // 未连接，尝试连接
        match controller.connect_async().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect: {e:?}");
                Timer::after(Duration::from_millis(5000)).await;  // 5秒后重试
            }
        }
    }
}
```

esp-radio 0.18 的事件监听 API 使用 `subscribe() + next_event_pure()` 模式，取代了旧版的 `wait_for_event()`。这种方式可以获取所有 WiFi 事件，而不只是特定的一种。

### 2.3 自动关闭机制

```rust
// src/wifi.rs:312-323
#[embassy_executor::task]
async fn do_stop() {
    loop {
        if let Some(WifiNetState::WifiConnected) = *WIFI_STATE.lock().await {
            // 30 秒没有网络请求，自动关闭 WiFi
            if Instant::now().as_secs() - LAST_USE_TIME_SECS.lock().await.unwrap() > 30 {
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
// src/wifi.rs:242-306（简化）
pub async fn use_wifi() -> Result<&'static Stack<'static>, WifiNetError> {
    // 等待获取锁
    while *WIFI_LOCK.lock().await {
        Timer::after(Duration::from_millis(500)).await;
    }
    *WIFI_LOCK.lock().await = true;  // 加锁

    // 确保 WiFi 已连接
    if *WIFI_STATE.lock().await == None {
        REINIT_WIFI_SIGNAL.signal(());  // 触发初始化
    }
    if WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
        RECONNECT_WIFI_SIGNAL.signal(());  // 触发重连
    }

    // 等待连接就绪
    loop {
        let stack = unsafe { *core::ptr::addr_of!(STACK_MUT) };
        if let Some(v) = stack {
            if v.is_link_up() {
                v.wait_config_up().await;
                return Ok(v);
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

## 3. HTTP/HTTPS 客户端

### 3.1 请求客户端

`RequestClient` 封装了 HTTP 和 HTTPS 请求，支持自动识别 URL 协议：

```rust
// src/request.rs:47-54
pub struct RequestClient {
    stack: &'static Stack<'static>,
    rng: RngWrapper,
    rx_buffer: Vec<u8>,       // 4KB TCP 接收缓冲区
    tx_buffer: Vec<u8>,       // 4KB TCP 发送缓冲区
    tls_rx_buffer: Vec<u8>,   // 4KB TLS 接收缓冲区
    tls_tx_buffer: Vec<u8>,   // 4KB TLS 发送缓冲区
}
```

内存开销总计：4 × 4KB = 16KB。在 80KB 堆上，这是一笔不小的开销，但足够处理天气 API 的 JSON 响应（通常 < 4KB）。

### 3.2 URL 解析

```rust
// src/request.rs:74-101
pub async fn send_request(&mut self, url: &str) -> Result<ResponseData, RequestError> {
    if let Some(rest) = url.strip_prefix("https://") {
        let (host_and_port, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = host_and_port.split_once(':').unwrap_or((host_and_port, "443"));
        self.send_https_request(url, host, port.parse()?, path).await
    } else if let Some(rest) = url.strip_prefix("http://") {
        let (host_and_port, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = host_and_port.split_once(':').unwrap_or((host_and_port, "80"));
        self.send_plain_http_request(url, host, port.parse()?, path).await
    } else {
        Err(RequestError::UnsupportedScheme)
    }
}
```

### 3.3 HTTPS 请求流程

HTTPS 请求在 TCP 之上加了 TLS 层：

```rust
// src/request.rs:148-209（简化）
async fn send_https_request(&mut self, host: &str, port: u16, path: &str) -> Result<...> {
    // 1. DNS 解析
    let ip_address = self.resolve(host).await?;

    // 2. TCP 连接
    let mut socket = TcpSocket::new(*self.stack, &mut self.rx_buffer, &mut self.tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(10)));
    socket.connect((ip_address, port)).await?;

    // 3. TLS 握手
    let config = TlsConfig::new()
        .with_server_name(host)
        .enable_rsa_signatures();
    let mut tls = TlsConnection::new(socket, &mut self.tls_rx_buffer, &mut self.tls_tx_buffer);
    tls.open(TlsContext::new(&config, UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng))).await?;

    // 4. 发送 HTTP 请求
    let request = Request::get(path).host(host).build();
    request.write_header(&mut tls).await?;

    // 5. 读取响应
    let response = Response::read(&mut tls, Method::GET, &mut headers_buf).await?;
    let total_length = response.body().reader().read_to_end(&mut buf).await?;

    // 6. 关闭连接
    tls.close().await;
    Ok(ResponseData { data: buf, length: total_length })
}
```

注意 TLS 使用 `UnsecureProvider`——这意味着不做证书验证。在嵌入式环境中，完整的 CA 证书链验证需要几百 KB 的存储空间来存放根证书，ESP32-C3 没有这么多富余内存。对于天气 API 这类非敏感数据，跳过验证是可接受的。

`Aes128GcmSha256` 指定了 TLS 加密套件，使用 AES-128-GCM 进行加密，SHA-256 进行完整性校验。

## 4. AP 模式与配网

首次使用时，设备没有 WiFi 配置。此时进入 AP（Access Point）模式，创建一个热点"esp_wifi"，手机连接后通过 Web 页面配置 WiFi 信息。

### 4.1 AP 模式初始化

```rust
// src/wifi.rs:367-437
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
    spawner.spawn(dhcp_service()).unwrap();  // 自建 DHCP 服务
    spawner.spawn(dns_service()).unwrap();   // 自建 DNS 服务
    spawner.spawn(connection_wifi_ap(controller)).unwrap();
}
```

### 4.2 自建 DHCP 服务

AP 模式下没有现成的 DHCP 服务器。本项目用 UDP socket 在 67 端口手动实现了 DHCP 协议的核心部分：

```rust
// src/wifi.rs:473-535（简化）
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
// src/wifi.rs:614-665（简化）
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
    // ... 复制查询，添加回答 ...
    response.extend_from_slice(&ip.octets()).unwrap();    // ESP32 的 IP
    response
}
```

这意味着用户在手机浏览器中输入任何网址（如 `www.baidu.com`），都会被解析到 ESP32 的 IP `192.168.2.1`，从而打开配网页面。这是一种常见的 Captive Portal 技术。

## 5. Web 配置服务

配网服务运行在 AP 模式下的 TCP 80 端口，提供 Web 页面和 REST API：

```rust
// src/web_service.rs:44-125（简化）
async fn web_tcp_socket(stack: &'static Stack<'static>) {
    let mut socket = TcpSocket::new(*stack, &mut rx_buffer, &mut tx_buffer);
    loop {
        socket.accept(IpListenEndpoint { addr: None, port: 80 });

        // Keep-alive：同一 TCP 连接处理多个 HTTP 请求
        loop {
            // 读取请求头
            // ...
            process_http(&mut socket, &buffer).await;
        }
        socket.close();
    }
}
```

支持的 API：

| 路径 | 方法 | 功能 |
|------|------|------|
| `/` | GET | 返回配网 HTML 页面 |
| `/configure_wifi` | POST | 保存 WiFi 配置并重启 |
| `/configure_weather` | POST | 保存天气 API 配置 |
| `/configure_sleep` | POST | 保存睡眠时间配置 |
| `/books` | GET | 列出 SD 卡中的电子书 |
| `/images` | GET | 列出 SD 卡中的图片 |
| `/upload?name=XX&chunk=N` | POST | 分块上传电子书 |
| `/upload_image?name=XX&chunk=N` | POST | 分块上传图片 |
| `/delete` | POST | 删除电子书 |
| `/delete_image` | POST | 删除图片 |
| `/sleep_image` | GET | 查询待机壁纸状态 |
| `/upload_sleep_image` | POST | 上传待机壁纸 |
| `/delete_sleep_image` | POST | 删除待机壁纸 |

文件上传支持分块（chunk），解决了嵌入式设备 TCP 缓冲区有限的问题。每块 512 字节，通过 URL 参数 `chunk=N` 指定序号。

## 小结

| 技术点 | 要点 |
|--------|------|
| WiFi 驱动 | `esp-radio` 0.18，`wifi::new()` 一行初始化 |
| 网络栈 | embassy-net 0.9，Stack/Runner 分离 |
| WiFi 生命周期 | 4 种状态 + Signal 驱动的状态转换 |
| 自动省电 | 30 秒无请求自动关闭 WiFi |
| HTTP | reqwless 0.14，GET 请求 |
| HTTPS | embedded-tls，AES-128-GCM，跳过证书验证 |
| AP 配网 | 自建 DHCP + DNS 劫持（Captive Portal） |
| Web 服务 | TCP 80 端口，Keep-alive，分块上传 |

在只有 400KB SRAM 的 ESP32-C3 上实现完整的 WiFi + TCP/IP + TLS + HTTP 协议栈，Embassy 的异步架构功不可没——它让网络操作不会阻塞 UI，而精细的 WiFi 生命周期管理确保了电池续航。

在下一篇文章中，我们将进入存储层，看看 Flash 持久化和 SD 卡文件系统是如何实现的。

---

> 上一篇：[第 4 篇：Embassy 异步运行时与事件驱动架构](04-Embassy异步运行时与事件驱动.md) · 下一篇：[第 6 篇：数据持久化与文件系统](06-数据持久化与文件系统.md)
