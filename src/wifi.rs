use alloc::string::ToString;
use core::net::Ipv4Addr;
use dhcparse::dhcpv4::{Addr, DhcpOption, Encode, Encoder, Message};
use dhcparse::v4_options;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_net::udp::{UdpSocket, UdpMetadata};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_println::println;
use esp_hal::rng::Rng;
use heapless::{String, Vec};
use static_cell::make_static;
use esp_radio::wifi::{
    AuthenticationMethod,
    Config as WifiConfig,
    ControllerConfig,
    Interface,
    WifiController,
    sta::StationConfig,
    ap::AccessPointConfig,
};


#[derive(Eq, PartialEq,Copy, Clone,Debug)]
pub enum WifiModel{
    AP,
    STA,
}
#[derive(Eq, PartialEq,Copy, Clone,Debug)]
pub enum WifiNetState {
    WifiConnecting,
    WifiConnected,
    WifiDisconnected,
    WifiStopped,
}
#[derive(Debug)]
pub enum WifiNetError {
    WaitConnecting,
    TimeOut,
    Infallible,
    Using,
}




const SSID: &str = match option_env!("SSID") {
    Some(v) => v,
    None => "",
};
const PASSWORD: &str = match option_env!("PASSWORD") {
    Some(v) => v,
    None => "",
};

const HOW_LONG_SECS_CLOSE:u64 = 30;
const MAX_CONNECT_RETRIES: u32 = 5;
const CONNECT_RETRY_INTERVAL_MS: u64 = 5000;
const INIT_WAIT_TIMEOUT_SECS: u64 = 35;

pub(crate) static mut IP_ADDRESS: String<20> = String::new();
pub static STOP_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static RECONNECT_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static REINIT_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static LAST_USE_TIME_SECS:Mutex<CriticalSectionRawMutex,Option<u64>>  =  Mutex::new(None);
pub static WIFI_STATE:Mutex<CriticalSectionRawMutex,Option<WifiNetState>>  =  Mutex::new(None);
pub(crate) static mut STACK_MUT: Option<&'static Stack<'static>> = None;
pub(crate) static mut AP_STACK_MUT: Option<&'static Stack<'static>> = None;

pub static HAL_RNG:Mutex<CriticalSectionRawMutex,Option<Rng>>  =  Mutex::new(None);


static mut REQUEST_LOADING: bool = false;

pub fn is_request_loading() -> bool {
    unsafe { *core::ptr::addr_of!(REQUEST_LOADING) }
}

pub fn set_request_loading(loading: bool) {
    unsafe { core::ptr::addr_of_mut!(REQUEST_LOADING).write(loading); }
}
pub static WIFI_MODEL:Mutex<CriticalSectionRawMutex,Option<WifiModel>> = Mutex::new(None);

pub async fn connect_wifi(spawner: &Spawner,
                          rng: Rng,
                          wifi: esp_hal::peripherals::WIFI<'static>,
    ) -> Result<&'static Stack<'static>, WifiNetError> {
    println!("wait init wifi");
    REINIT_WIFI_SIGNAL.wait().await;

    println!("init wifi");
    HAL_RNG.lock().await.replace(rng);

    let ssid = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_ssid.clone();
    let password = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_password.clone();

    let station_config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(ssid.as_str())
            .with_password(password.as_str().into()),
    );

    let (controller, interfaces) = esp_radio::wifi::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    ).unwrap();

    let wifi_interface = interfaces.station;

    let config = Config::dhcpv4(Default::default());

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (stack, runner) = embassy_net::new(
        wifi_interface,
        config,
        make_static!(StackResources::<4>::new()),
        seed
    );
    let stack: &Stack<'static> = &*make_static!(stack);

    refresh_last_time().await;

    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(connection_wifi(controller).unwrap());
    spawner.spawn(do_stop().unwrap());
    let _ = spawner;
    let init_start_secs = Instant::now().as_secs();
    loop {
        println!("Waiting is_link_up...");
        if stack.is_link_up() {
            break;
        }
        if matches!(*WIFI_STATE.lock().await, Some(WifiNetState::WifiStopped)) {
            println!("wifi gave up during init");
            return Err(WifiNetError::TimeOut);
        }
        if Instant::now().as_secs() - init_start_secs > INIT_WAIT_TIMEOUT_SECS {
            println!("wifi init timeout (link)");
            return Err(WifiNetError::TimeOut);
        }
        Timer::after(Duration::from_millis(1000)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            unsafe {
                *core::ptr::addr_of_mut!(IP_ADDRESS) = config.address.address().to_string().parse().unwrap();
            }
            break;
        }
        if Instant::now().as_secs() - init_start_secs > INIT_WAIT_TIMEOUT_SECS + 10 {
            println!("wifi init timeout (dhcp)");
            return Err(WifiNetError::TimeOut);
        }
        Timer::after(Duration::from_millis(500)).await;
    }
    unsafe {
        core::ptr::addr_of_mut!(STACK_MUT).write(Some(stack));
    }
    Ok(stack)
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn connection_wifi(mut controller: WifiController<'static>) {
    println!("start connection task1");
    let mut consecutive_failures: u32 = 0;
    loop {
        println!("loop");
        if controller.is_connected() {
            consecutive_failures = 0;
            WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);

            // Wait for either disconnect or stop signal
            // Poll for disconnect event or stop signal
            loop {
                let mut subscriber = controller.subscribe().unwrap();
                let close_signal = STOP_WIFI_SIGNAL.wait();
                match select(subscriber.next_event_pure(), close_signal).await {
                    Either::First(_) => {
                        // Check if we got a disconnect
                        drop(subscriber);
                        if !controller.is_connected() {
                            WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                            Timer::after(Duration::from_millis(1000)).await;
                            println!("wifi disconnected...");
                            break;
                        }
                    }
                    Either::Second(_) => {
                        drop(subscriber);
                        STOP_WIFI_SIGNAL.reset();
                        let _ = controller.disconnect_async().await;
                        println!("wifi close...");
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiStopped);
                        RECONNECT_WIFI_SIGNAL.wait().await;
                        RECONNECT_WIFI_SIGNAL.reset();
                        println!("restart connect...");
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                        break;
                    }
                }
            }
        } else {
            WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
        }

        let ssid = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_ssid.clone();
        let password = crate::storage::WIFI_INFO.lock().await.as_ref().unwrap().wifi_password.clone();
        println!("ssid: {}", ssid);

        let station_config = WifiConfig::Station(
            StationConfig::default()
                .with_ssid(ssid.as_str())
                .with_password(password.as_str().into()),
        );
        match controller.set_config(&station_config) {
            Ok(_) => {}
            Err(e) => {
                println!("config error: {:?}", e);
            }
        }

        println!("About to connect...");

        WIFI_STATE.lock().await.replace(WifiNetState::WifiConnecting);
        match controller.connect_async().await {
            Ok(_) => {
                println!("Wifi connected!");
                consecutive_failures = 0;
                WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);
            },
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONNECT_RETRIES {
                    println!(
                        "wifi give up after {} attempts, will sleep",
                        consecutive_failures
                    );
                    consecutive_failures = 0;
                    let _ = controller.disconnect_async().await;
                    WIFI_STATE.lock().await.replace(WifiNetState::WifiStopped);
                    RECONNECT_WIFI_SIGNAL.wait().await;
                    RECONNECT_WIFI_SIGNAL.reset();
                    println!("restart connect after give-up...");
                    WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                } else {
                    Timer::after(Duration::from_millis(CONNECT_RETRY_INTERVAL_MS)).await;
                }
            }
        }
    }
}

pub async fn refresh_last_time(){
    // 仅刷新“网络使用时间”（供 do_stop 在 30s 空闲后自动关 WiFi）。
    // 注意：这里不再调用 refresh_active_time()。后台任务（ntp_worker 等）调用
    // use_wifi 时若复位 LAST_ACTIVE_TIME，断网场景下 NTP 会无限重试，每 ~11s
    // 复位一次用户空闲计时，导致 to_sleep_tips 的 180s 空闲判断永远不成立、
    // 设备永远无法睡眠。用户活动时间只应由真实的用户操作（按键/页面）来刷新。
    LAST_USE_TIME_SECS.lock().await.replace(Instant::now().as_secs());
}


const TIME_OUT_SECS: u64 = 10;
static WIFI_LOCK:Mutex<CriticalSectionRawMutex,bool> = Mutex::new(false);
pub async fn use_wifi() ->Result<&'static Stack<'static>, WifiNetError>{
    let secs = Instant::now().as_secs();
    loop {
        if !*WIFI_LOCK.lock().await  {
            break;
        }
        if Instant::now().as_secs() - secs > TIME_OUT_SECS  {
            return Err(WifiNetError::TimeOut);
        }
        // 等待他人释放锁期间不刷新网络使用时间：锁持有者自己会刷。
        // 否则 force_stop_wifi（睡眠前关 WiFi）会因 LAST_USE_TIME_SECS 一直被
        // 等锁者刷新，误判“仍有请求进行中”而永久卡在等待循环里，导致睡不下去。
        Timer::after(Duration::from_millis(500)).await;
    }
    // 锁已独占。关键：拿到锁之后的任何失败路径都必须释放锁，否则 WIFI_LOCK 被永久卡死——
    // 后续所有 use_wifi 都会超时失败，force_stop_wifi 也会一直等不到锁释放而无法睡眠。
    *WIFI_LOCK.lock().await = true;

    let result = use_wifi_locked(secs).await;
    if result.is_err() {
        *WIFI_LOCK.lock().await = false;
        println!("use_wifi failed, wifi lock released");
    }
    result
}

/// use_wifi 拿到 WIFI_LOCK 之后的核心逻辑。调用方（use_wifi）负责在 Err 时释放锁。
async fn use_wifi_locked(secs: u64) -> Result<&'static Stack<'static>, WifiNetError> {
    println!("wifi state: {:?}",*WIFI_STATE.lock().await);
    if *WIFI_STATE.lock().await == None {

        println!("need init wifi");
        REINIT_WIFI_SIGNAL.signal(());
        loop {
            refresh_last_time().await;
            if *WIFI_STATE.lock().await != None { break; }
            if Instant::now().as_secs() - secs > 3 {
                return Err(WifiNetError::WaitConnecting);
            }
            Timer::after_millis(500).await;
        }
    }
    if WIFI_STATE.lock().await.unwrap() != WifiNetState::WifiConnected {
        println!("need wait");
    }
    if WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
        println!("send reconnect signal...");
        RECONNECT_WIFI_SIGNAL.signal(());
    }


    let mut try_times = 10;
    loop {
        refresh_last_time().await;
        println!("use_wifi Waiting is_link_up...");
        unsafe {
            let stack = *core::ptr::addr_of!(STACK_MUT);
            if let Some(v) = stack {
                if v.is_link_up() {
                    v.wait_config_up().await;
                    return Ok(v);
                } else if Instant::now().as_secs() - secs > TIME_OUT_SECS {
                    return Err(WifiNetError::TimeOut);
                }
            } else if try_times == 0 {
                return Err(WifiNetError::Infallible);
            }

            try_times -= 1;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

pub async fn finish_wifi(){
    refresh_last_time().await;
    *WIFI_LOCK.lock().await   = false;
    println!("finish_wifi");
}

pub async fn wifi_is_idle()->bool{
     !*WIFI_LOCK.lock().await
}

#[embassy_executor::task]
async fn do_stop(){
    loop {
        if  let Some(WifiNetState::WifiConnected)  = *WIFI_STATE.lock().await {
            // 有请求进行中（WIFI_LOCK 被持有）时不要关 WiFi，避免打断未完成的请求；
            // 单凭 LAST_USE_TIME_SECS 不够：socket 长读期间它不会被刷新，会误判为可关闭。
            if Instant::now().as_secs() - LAST_USE_TIME_SECS.lock().await.unwrap() > HOW_LONG_SECS_CLOSE
                && !*WIFI_LOCK.lock().await {
                println!("do_stop_wifi");
                STOP_WIFI_SIGNAL.signal(());
            }
        }
        Timer::after(Duration::from_millis(3000)).await
    }
}

pub async fn force_stop_wifi(){
    println!("force_stop_wifi:{:?}", *WIFI_STATE.lock().await);
    if *WIFI_STATE.lock().await == None {
        return;
    }

    if  WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
        return;
    }

    // 关键：若有网络请求正在进行（WIFI_LOCK 被持有），必须等其自然完成或超时，
    // 绝不抢占式强杀 WiFi——否则会打断尚未完成的请求（含连接失败、仍在 socket 读
    // 超时窗口内的请求）。请求侧自身有界：use_wifi 等链路最多 ~10s、socket 超时 10s，
    // 单次请求无论成功失败都会在 ~30s 内走 finish_wifi 释放锁。
    // 这里 45s 仅为兜底：正常永远不会触发，只有请求真正 hung 才强制停止以防睡死。
    let wait_start = Instant::now().as_secs();
    loop {
        if !*WIFI_LOCK.lock().await {
            break;
        }
        if Instant::now().as_secs() - wait_start > 45 {
            println!("wifi request held lock >45s (hung?), force stop anyway");
            break;
        }
        println!("wait in-flight wifi request to finish");
        Timer::after(Duration::from_millis(200)).await;
    }

    // 占住锁，阻止关 WiFi 期间发起新请求
    *WIFI_LOCK.lock().await = true;

    STOP_WIFI_SIGNAL.signal(());
    loop {
        if  WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
            return;
        }
        Timer::after(Duration::from_millis(50)).await
    }
}

/// AP mode
pub async fn start_wifi_ap(spawner: &Spawner,
                           rng: Rng,
                           wifi: esp_hal::peripherals::WIFI<'static>,
    ) -> Result<&'static Stack<'static>, WifiNetError> {

    HAL_RNG.lock().await.replace(rng);

    let ap_config = WifiConfig::AccessPoint(
        AccessPointConfig::default()
            .with_ssid("esp_wifi")
    );

    let config = ControllerConfig::default().with_initial_config(ap_config);
    let (controller, interfaces) = match esp_radio::wifi::new(wifi, config) {
        Ok(result) => result,
        Err(e) => {
            println!("!! wifi::new1111() failed: {:?}", e);
            panic!("wifi new failed");
        }
    };

    let wifi_ap_interface = interfaces.access_point;

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let ap_net_config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 2, 1), 24),
        gateway: Some(Ipv4Address::new(192, 168, 2, 1)),
        dns_servers: Default::default(),
    });

    let (ap_stack, runner) = embassy_net::new(
        wifi_ap_interface,
        ap_net_config,
        make_static!(StackResources::<4>::new()),
        seed
    );
    let ap_stack: &Stack<'static> = &*make_static!(ap_stack);

    spawner.spawn(ap_task(runner).unwrap());
    spawner.spawn(dhcp_service().unwrap());
    spawner.spawn(dns_service().unwrap());
    spawner.spawn(connection_wifi_ap(controller).unwrap());
    let _ = spawner;

    loop {
        println!("Waiting is_link_up...");
        if ap_stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(1000)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = ap_stack.config_v4() {
            println!("Got IP: {}", config.address);
            unsafe {
                *core::ptr::addr_of_mut!(IP_ADDRESS) = config.address.address().to_string().parse().unwrap();
            }
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
    unsafe {
        core::ptr::addr_of_mut!(AP_STACK_MUT).write(Some(ap_stack));
    }

    Ok(ap_stack)
}

#[embassy_executor::task]
async fn ap_task(mut runner: embassy_net::Runner<'static, Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn connection_wifi_ap(mut controller: WifiController<'static>) {
    println!("start connection task");
    let ap_config = WifiConfig::AccessPoint(
        AccessPointConfig::default()
            .with_ssid("esp_wifi")
    );
    match controller.set_config(&ap_config) {
        Ok(_) => {
            println!("AP config set successfully");
        }
        Err(e) => {
            println!("Error setting Wifi configuration: {:?}", e);
        }
    }
    loop {
        if controller.is_connected() {
            // AP is running, wait for stop event
            let mut subscriber = controller.subscribe().unwrap();
            subscriber.next_event_pure().await;
            if !controller.is_connected() {
                println!("AP stopped, restarting...");
            }
        } else {
            Timer::after(Duration::from_millis(5000)).await;
        }
    }
}

#[embassy_executor::task]
async fn dhcp_service(){
    const RX_BUFFER_SIZE: usize = 512;
    const TX_BUFFER_SIZE: usize = 512;
    const PACKET_META_SIZE: usize = 10;


    loop {
        unsafe {
            let ap_stack = *core::ptr::addr_of!(AP_STACK_MUT);
            if let Some(ap_stack) = ap_stack {
                loop {
                    if ap_stack.is_link_up() {
                        break;
                    }
                    Timer::after(Duration::from_millis(500)).await;
                }

                let mut rx_meta = [embassy_net::udp::PacketMetadata::EMPTY; PACKET_META_SIZE];
                let mut rx_buffer = [0u8; RX_BUFFER_SIZE];
                let mut tx_meta = [embassy_net::udp::PacketMetadata::EMPTY; PACKET_META_SIZE];
                let mut tx_buffer = [0u8; TX_BUFFER_SIZE];
                let mut udp_socket = UdpSocket::new(*ap_stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
                udp_socket.bind(67);

                loop {
                    let mut buf = [0u8; 512];
                    println!("等待请求") ;
                    match udp_socket.recv_from(&mut buf).await {
                        Ok((n, src)) => {
                            println!("Received {} bytes from {}", n, src);
                            println!("Received:{:?} ", buf );

                            let msg = Message::new(buf).unwrap();
                            println!("msg op_type:{:?}",msg.op().unwrap()) ;
                            let options =  v4_options!(msg; MessageType required, ServerIdentifier, RequestedIpAddress);
                            match options {
                                Ok((msg_type,_si,_ria)) => {
                                    println!("msg type:{:?}",msg_type) ;
                                    if msg_type == dhcparse::dhcpv4::MessageType::DISCOVER {
                                        send_dhcp_offer(&udp_socket, src ,&msg).await;
                                    }
                                    else if msg_type ==  dhcparse::dhcpv4::MessageType::REQUEST {
                                        let _ip_addr = Ipv4Addr::new(192, 168, 2, 2);
                                        send_dhcp_ack(&udp_socket, src, &msg).await;
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                        Err(e) => {
                            println!("Failed to receive UDP packet: {:?}", e);
                        }
                    }

                    Timer::after(Duration::from_secs(1)).await;
                }
            }
        }

        Timer::after(Duration::from_millis(500)).await
    }
}

async fn send_dhcp_offer(udp_socket: &UdpSocket<'_>, _src_addr: UdpMetadata, receive_msg: &Message<[u8; 512]>) {
    println!("send_dhcp_offer") ;
    let router_ip:&Addr = (&[192u8,168,2,1][..]).try_into().unwrap();
    let submask:&Addr = (&[255u8,255,255,0][..]).try_into().unwrap();

    let mut offer_message = [0u8; 512];

    offer_message[2] = 6;

    let mut msg = Encoder
        .append_options([DhcpOption::MessageType(dhcparse::dhcpv4::MessageType::OFFER)])
        .append_options([DhcpOption::Router(&[*router_ip])])
        .append_options([DhcpOption::SubnetMask(submask)])
        .append_options([DhcpOption::AddressLeaseTime(3600)])
        .append_options([DhcpOption::ServerIdentifier(router_ip)])
        .append_options([DhcpOption::DomainNameServer(&[*router_ip])])
        .encode(&Message::default(), &mut offer_message).unwrap();
    msg.set_op(dhcparse::dhcpv4::OpCode::BootReply);
    msg.set_xid(receive_msg.xid());
    msg.set_chaddr(receive_msg.chaddr().unwrap());


    let temp :[u8;4] = [192,168,2,1];
    let si_addr:&Addr = (&temp[..]).try_into().unwrap();
    *msg.siaddr_mut() = *si_addr;


    let temp :[u8;4] = [192,168,2,2];
    let yi_addr:&Addr = (&temp[..]).try_into().unwrap();
    *msg.yiaddr_mut() = *yi_addr;

    offer_message[1] = 1;
    println!("{:?}",&offer_message);


    let broadcast = ( Ipv4Address::BROADCAST,68);
    let _ = udp_socket.send_to(&offer_message, broadcast).await;
}

async fn send_dhcp_ack(udp_socket: & UdpSocket<'_>, _src_addr: UdpMetadata, receive_msg: &Message<[u8; 512]>) {
    println!("send_dhcp_ack") ;
    let router_ip:&Addr = (&[192u8,168,2,1][..]).try_into().unwrap();
    let submask:&Addr = (&[255u8,255,255,0][..]).try_into().unwrap();
    let mut offer_message = [0u8; 512];
    offer_message[1] = 1;
    offer_message[2] = 6;

    let mut msg = Encoder
        .append_options([DhcpOption::MessageType(dhcparse::dhcpv4::MessageType::ACK)])
        .append_options([DhcpOption::Router(&[*router_ip])])
        .append_options([DhcpOption::SubnetMask(submask)])
        .append_options([DhcpOption::AddressLeaseTime(3600)])
        .append_options([DhcpOption::ServerIdentifier(router_ip)])
        .append_options([DhcpOption::DomainNameServer(&[*router_ip])])
        .encode(&Message::default(), &mut offer_message).unwrap();
    msg.set_op(dhcparse::dhcpv4::OpCode::BootReply);
    msg.set_xid(receive_msg.xid());
    msg.set_chaddr(receive_msg.chaddr().unwrap());

    let temp :[u8;4] = [192,168,2,1];
    let si_addr:&Addr = (&temp[..]).try_into().unwrap();
    *msg.siaddr_mut() = *si_addr;



    let temp :[u8;4] = [192,168,2,2];
    let yi_addr:&Addr = (&temp[..]).try_into().unwrap();
    *msg.yiaddr_mut() = *yi_addr;

    offer_message[1] = 1;
    println!("{:?}",&offer_message);

    let broadcast = ( Ipv4Address::BROADCAST,68);
    let _ = udp_socket.send_to(&offer_message, broadcast).await;
}

//DNS劫持服务
#[embassy_executor::task]
async fn dns_service(){
    const RX_BUFFER_SIZE: usize = 512;
    const TX_BUFFER_SIZE: usize = 512;
    const PACKET_META_SIZE: usize = 10;


    const LOCAL_IP:Ipv4Addr =  Ipv4Addr::new(192, 168, 2, 1);

    'main_loop: loop {
        unsafe {
            let ap_stack = *core::ptr::addr_of!(AP_STACK_MUT);
            if let Some(ap_stack) = ap_stack {
                loop {
                    if ap_stack.is_link_up() {
                        break;
                    }
                    Timer::after(Duration::from_millis(500)).await;
                }

                let mut rx_meta = [embassy_net::udp::PacketMetadata::EMPTY; PACKET_META_SIZE];
                let mut rx_buffer = [0u8; RX_BUFFER_SIZE];
                let mut tx_meta = [embassy_net::udp::PacketMetadata::EMPTY; PACKET_META_SIZE];
                let mut tx_buffer = [0u8; TX_BUFFER_SIZE];
                let mut udp_socket = UdpSocket::new(*ap_stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
                udp_socket.bind(53);

                loop {
                    let mut buf = [0u8; 512];
                    println!("Dns等待请求") ;
                    match udp_socket.recv_from(&mut buf).await {
                        Ok((n, src)) => {
                            println!("Dns Received {} bytes from {}", n, src);
                            println!("Dns Received:{:?} ", buf );

                            let response = create_dns_response(LOCAL_IP,&buf[..n]);
                            let _ = udp_socket.send_to(&response, src).await;
                        }
                        Err(e) => {
                            println!("Failed to receive UDP packet: {:?}", e);
                        }
                    }


                    Timer::after(Duration::from_millis(50)).await
                }
            }
        }

        Timer::after(Duration::from_millis(500)).await
    }
}

fn create_dns_response(ip:Ipv4Addr, request: &[u8]) -> Vec<u8, 512> {
    let mut response = Vec::new();

    response.extend_from_slice(&request[0..2]).unwrap();
    response.extend_from_slice(&[0x81, 0x80]).unwrap();
    response.extend_from_slice(&request[4..6]).unwrap();
    response.extend_from_slice(&[0x00, 0x01]).unwrap();
    response.extend_from_slice(&[0x00, 0x00]).unwrap();
    response.extend_from_slice(&[0x00, 0x00]).unwrap();
    response.extend_from_slice(&request[12..]).unwrap();
    response.extend_from_slice(&[0xc0, 0x0c]).unwrap();
    response.extend_from_slice(&[0x00, 0x01]).unwrap();
    response.extend_from_slice(&[0x00, 0x01]).unwrap();
    response.extend_from_slice(&[0x00, 0x00, 0x00, 0x3c]).unwrap();
    response.extend_from_slice(&[0x00, 0x04]).unwrap();
    response.extend_from_slice(&ip.octets()).unwrap();

    response
}
