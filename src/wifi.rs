use alloc::string::ToString;
use core::net::Ipv4Addr;
use core::ops::{Deref, DerefMut};
use core::str::{from_utf8, FromStr};
use dhcparse::dhcpv4::{Addr, DhcpOption, Encode, Encoder, Message};
use dhcparse::v4_options;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Config, IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_net::tcp::{AcceptError, TcpSocket};
use embassy_net::udp::UdpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_println::{print, println};
use esp_storage::FlashStorageError;
use esp_wifi::{EspWifiInitFor, initialize};
use esp_wifi::wifi::{AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration, WifiApDevice, WifiController, WifiDevice, WifiError, WifiEvent, WifiStaDevice, WifiState};
use esp_wifi::wifi::ipv4::{ RouterConfiguration, SocketAddrV4};
use esp_hal::clock::Clocks;
use esp_hal::peripherals::{RADIO_CLK, SYSTIMER, TIMG0, WIFI};
use esp_hal::reset::software_reset;
use esp_hal::rng::Rng;
use esp_hal::system::SystemClockControl;
use esp_hal::timer::PeriodicTimer;
use heapless::{String, Vec};
use httparse::Header;
use static_cell::{ StaticCell};
use crate::make_static;


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




const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

const HOW_LONG_SECS_CLOSE:u64 = 30;//20秒未使用wifi 断开

pub static mut IP_ADDRESS:String<20> = String::new();
pub static STOP_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static RECONNECT_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static REINIT_WIFI_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static LAST_USE_TIME_SECS:Mutex<CriticalSectionRawMutex,Option<u64>>  =  Mutex::new(None);
pub static WIFI_STATE:Mutex<CriticalSectionRawMutex,Option<WifiNetState>>  =  Mutex::new(None);
pub static mut STACK_MUT: Option<&'static Stack<WifiDevice<'static, WifiStaDevice>>>  =  None;
pub static mut AP_STACK_MUT: Option<&'static Stack<WifiDevice<'static, WifiApDevice>>>  =  None;

pub static HAL_RNG:Mutex<CriticalSectionRawMutex,Option<Rng>>  =  Mutex::new(None);
pub static WIFI_MODEL:Mutex<CriticalSectionRawMutex,Option<WifiModel>> = Mutex::new(None);
pub async fn connect_wifi(spawner: &Spawner,
                          timg0: TIMG0,
                          rng: Rng,
                          wifi: WIFI,
                          radio_clk: RADIO_CLK,
                          clocks: &Clocks<'_> )
    -> Result<&'static Stack<WifiDevice<'static, WifiStaDevice>>, WifiNetError> {
    println!("wait init wifi");
    REINIT_WIFI_SIGNAL.wait().await;

    println!("init wifi");
    HAL_RNG.lock().await.replace(rng);

    let timer = esp_hal::timer::timg::TimerGroup::new(timg0, &clocks);



    let init = initialize(
        EspWifiInitFor::Wifi,
        timer.timer0,
        rng,
        radio_clk,
        &clocks,
    )
        .unwrap();


    let (wifi_interface, controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();


    let config = Config::dhcpv4(Default::default());

    let seed = 1234;

    // Init network stack
    let stack = &*make_static!(
        Stack<WifiDevice<'_, WifiStaDevice>>,
        Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>,StackResources::<3>::new()),
        seed
    ));

    refresh_last_time().await;

    spawner.spawn(connection_wifi(controller)).ok();
    spawner.spawn(net_task(stack)).ok();
    spawner.spawn(do_stop()).ok();
    loop {
        println!("Waiting is_link_up...");
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(1000)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            unsafe {
                IP_ADDRESS =  config.address.address().to_string().parse().unwrap();
            }
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
    unsafe {
        STACK_MUT = Some(stack);
    }
    Ok(stack)
}

#[embassy_executor::task]
async fn ap_task(stack: &'static Stack<WifiDevice<'static, WifiApDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn connection_wifi(mut controller: WifiController<'static>) {
    println!("start connection task1");
    println!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        println!("loop");
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::StaConnected => {
                // wait until we're no longer connected
                WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);
                let disconnect =  controller.wait_for_event(WifiEvent::StaDisconnected);
                let closeconnect = STOP_WIFI_SIGNAL.wait();

                match select(disconnect,closeconnect).await {
                    Either::First(_) => {
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);
                        Timer::after(Duration::from_millis(1000)).await
                    }
                    Either::Second(_) => {
                        STOP_WIFI_SIGNAL.reset();
                        controller.stop().await.expect("wifi stop error");
                        println!("wifi close...");
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiStopped);
                        RECONNECT_WIFI_SIGNAL.wait().await;
                        RECONNECT_WIFI_SIGNAL.reset();
                        println!("restart connect...");
                        WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);

                    }
                }

            }
            _ => { WIFI_STATE.lock().await.replace(WifiNetState::WifiDisconnected);}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            loop {

                let client_config = Configuration::Client(ClientConfiguration {
                    ssid:SSID.try_into().unwrap(),//wifi_info.wifi_ssid.clone(), //SSID.try_into().unwrap(),
                    password:PASSWORD.try_into().unwrap(),//wifi_info.wifi_password.clone(), //PASSWORD.try_into().unwrap(),
                    ..Default::default()
                });
                match controller.set_configuration(&client_config) {
                    Ok(_) => {}
                    Err(e) => {
                        println!("配置失败：{:?}",e);
                    }
                }
                println!("Starting wifi");
                controller.start().await.unwrap();
                println!("Wifi started!");
                break;

                Timer::after(Duration::from_millis(50)).await;

            }
        }
        println!("About to connect...");

        WIFI_STATE.lock().await.replace(WifiNetState::WifiConnecting);
        match controller.connect().await {
            Ok(_) =>{
                println!("Wifi connected!");
                WIFI_STATE.lock().await.replace(WifiNetState::WifiConnected);

            },
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

pub async fn refresh_last_time(){
    LAST_USE_TIME_SECS.lock().await.replace(Instant::now().as_secs());
    crate::sleep::refresh_active_time().await;
}


//每次请求都要获取stack 并修改 WIFI_LOCK 为true,用完后 改回，同时多个任务容易崩
const TIME_OUT_SECS: u64 = 10;
static WIFI_LOCK:Mutex<CriticalSectionRawMutex,bool> = Mutex::new(false);
pub async fn use_wifi() ->Result<&'static Stack<WifiDevice<'static, WifiStaDevice>>, WifiNetError>{
    let secs = Instant::now().as_secs();
    loop {
        if !*WIFI_LOCK.lock().await  {
            break;
        }
        if Instant::now().as_secs() - secs > TIME_OUT_SECS  {
            return Err(WifiNetError::TimeOut);
        }
        refresh_last_time().await;
        Timer::after(Duration::from_millis(50)).await;
    }
    *WIFI_LOCK.lock().await = true;

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
            Timer::after_millis(10).await;
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
            if let Some(v) = STACK_MUT {
                if v.is_link_up() {

                    v.wait_config_up().await;
                    return Ok(v);

                }else if Instant::now().as_secs() - secs > TIME_OUT_SECS {
                    return Err(WifiNetError::TimeOut);
                }
            }else if try_times == 0 {
                return Err(WifiNetError::Infallible);
            }

            try_times-=1;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

pub async fn finish_wifi(){
    refresh_last_time().await;
    *WIFI_LOCK.lock().await   = false;
    println!("finish_wifi");
}


#[embassy_executor::task]
async fn do_stop(){
    loop {
        if  let Some(WifiNetState::WifiConnected)  = *WIFI_STATE.lock().await {
            if Instant::now().as_secs() - LAST_USE_TIME_SECS.lock().await.unwrap() > HOW_LONG_SECS_CLOSE {
                println!("do_stop_wifi");
                STOP_WIFI_SIGNAL.signal(());
                //finish_wifi().await;
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
    }else{
        loop {
            println!("wait unlock wifi");
            if !*WIFI_LOCK.lock().await  {
                *WIFI_LOCK.lock().await =  true;
                break;
            }
            Timer::after(Duration::from_millis(50)).await;
        }
        
        //等待任务完成
        loop  {

            println!("current_secs:{}",Instant::now().as_secs());
            println!("last_secs:{}",LAST_USE_TIME_SECS.lock().await.unwrap());
            
            //wifi 最后更新时间要大于5s 才停止
            if Instant::now().as_secs() - LAST_USE_TIME_SECS.lock().await.unwrap() >  5{
                break;
            }else{
                println!("wait finish wifi");
                Timer::after(Duration::from_secs(1)).await;
            }
            
        }
        STOP_WIFI_SIGNAL.signal(());
        loop {
            if  WIFI_STATE.lock().await.unwrap() == WifiNetState::WifiStopped {
                return;
            }
            Timer::after(Duration::from_millis(50)).await
        }
    }
}

/// ap 模式 配网
pub async fn start_wifi_ap(spawner: &Spawner,
                           timg0: TIMG0,
                           rng: Rng,
                           wifi: WIFI,
                           radio_clk: RADIO_CLK,
                           clocks: &Clocks<'_> )
                           -> Result<&'static Stack<WifiDevice<'static, WifiApDevice>>, WifiNetError> {

    HAL_RNG.lock().await.replace(rng);

    let timer =
        esp_hal::timer::timg::TimerGroup::new(timg0, &clocks);

    let init = initialize(
        EspWifiInitFor::Wifi,
        timer.timer0,
        rng,
        radio_clk,
        &clocks,
    )
        .unwrap();

    let (wifi_ap_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiApDevice).unwrap();

    let seed = 1234;
    let ap_config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 2, 1), 24),
        gateway: Some(Ipv4Address::from_bytes(&[192, 168, 2, 1])),
        dns_servers: Default::default(),
    });
    let ap_stack: &Stack<WifiDevice<'static, WifiApDevice>> = &*make_static!(
         Stack<WifiDevice<'_, WifiApDevice>>,
            Stack::new(
                wifi_ap_interface,
                ap_config,
                make_static!(StackResources::<4>, StackResources::<4>::new()),
                seed
            )
        );

    spawner.spawn(ap_task(&ap_stack)).ok();
    spawner.spawn(dhcp_service()).ok();
    spawner.spawn(dns_service()).ok();
    spawner.spawn(connection_wifi_ap(controller)).ok();




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
                IP_ADDRESS =  config.address.address().to_string().parse().unwrap();
            }
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
    unsafe {
        AP_STACK_MUT = Some(ap_stack);
    }

    Ok(ap_stack)
}

#[embassy_executor::task]
async fn connection_wifi_ap(mut controller: WifiController<'static>) {
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::ApStarted => {
                // wait until we're no longer connected
                controller.wait_for_event(WifiEvent::ApStop).await;
                Timer::after(Duration::from_millis(5000)).await
            }
            _ => {}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::AccessPoint(AccessPointConfiguration {
                ssid: "esp-wifi".try_into().unwrap(),
                password:String::from_str("123456789").unwrap(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            println!("Starting wifi");
            controller.start().await.unwrap();
            println!("Wifi started!");
        }
    }
}

#[embassy_executor::task]
async fn dhcp_service(){
    const RX_BUFFER_SIZE: usize = 512; // 接收缓冲区大小
    const TX_BUFFER_SIZE: usize = 512; // 发送缓冲区大小
    const PACKET_META_SIZE: usize = 10; // 元数据大小


    loop {

        unsafe {
            if let Some(ap_stack) = AP_STACK_MUT {
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
                let mut udp_socket = UdpSocket::new(ap_stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
                udp_socket.bind(67);

                // 无限循环处理消息
                loop {
                    let mut buf = [0u8; 512];
                    println!("等待请求") ;
                    match udp_socket.recv_from(&mut buf).await {
                        Ok((n, src)) => {
                            println!("Received {} bytes from {}", n, src);
                            println!("Received:{:?} ", buf );

                            let mut msg = Message::new(buf).unwrap();
                            println!("msg op_type:{:?}",msg.op().unwrap()) ;
                            let options =  v4_options!(msg; MessageType required, ServerIdentifier, RequestedIpAddress);
                            match options {
                                Ok((msg_type,si,ria)) => {
                                    println!("msg type:{:?}",msg_type) ;
                                    if msg_type == dhcparse::dhcpv4::MessageType::DISCOVER {
                                        send_dhcp_offer(&udp_socket, src ,&msg).await;
                                    }
                                    else if msg_type ==  dhcparse::dhcpv4::MessageType::REQUEST {
                                        let ip_addr = Ipv4Addr::new(192, 168, 2, 2);
                                        send_dhcp_ack(&udp_socket, src, &msg).await;
                                    }
                                }
                                Err(_) => {}
                            }

                            //udp_socket.send_to(&buf[..n], src).await;
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

async fn send_dhcp_offer(udp_socket: &UdpSocket<'_>, src_addr: IpEndpoint, receive_msg: &Message<[u8; 512]>) {
    println!("send_dhcp_offer") ;
    // 构造并发送 DHCP Offer 消息
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
    udp_socket.send_to(&offer_message, broadcast).await;
}

async fn send_dhcp_ack(udp_socket: & UdpSocket<'_>, src_addr: IpEndpoint, receive_msg: &Message<[u8; 512]>) {
    println!("send_dhcp_ack") ;
    // 构造并发送 DHCP Acknowledge 消息
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
    udp_socket.send_to(&offer_message, broadcast).await;
}

//dns劫持服务
#[embassy_executor::task]
async fn dns_service(){
    const RX_BUFFER_SIZE: usize = 512; // 接收缓冲区大小
    const TX_BUFFER_SIZE: usize = 512; // 发送缓冲区大小
    const PACKET_META_SIZE: usize = 10; // 元数据大小


    const LOCAL_IP:Ipv4Addr =  Ipv4Addr::new(192, 168, 2, 1);

    'main_loop: loop {

        unsafe {
            if let Some(ap_stack) = AP_STACK_MUT {
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
                let mut udp_socket = UdpSocket::new(ap_stack, &mut rx_meta, &mut rx_buffer, &mut tx_meta, &mut tx_buffer);
                udp_socket.bind(53);

                // 无限循环处理消息
                loop {
                    let mut buf = [0u8; 512];
                    println!("Dns等待请求") ;
                    match udp_socket.recv_from(&mut buf).await {
                        Ok((n, src)) => {
                            println!("Dns Received {} bytes from {}", n, src);
                            println!("Dns Received:{:?} ", buf );

                            let response = create_dns_response(LOCAL_IP,&buf[..n]);
                            udp_socket.send_to(&response, src).await.expect("发送数据失败");
                            //break 'main_loop;

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

    // 构建简单的 DNS 响应，将所有请求重定向到 ESP32 的 IP 地址
    // 假设 DNS 请求符合规范并且无错误处理

    response.extend_from_slice(&request[0..2]).unwrap(); // 复制 ID
    response.extend_from_slice(&[0x81, 0x80]).unwrap(); // 标志：响应，无错误
    response.extend_from_slice(&request[4..6]).unwrap(); // 问题数
    response.extend_from_slice(&[0x00, 0x01]).unwrap(); // 答案数：1
    response.extend_from_slice(&[0x00, 0x00]).unwrap(); // 权威答案数：0
    response.extend_from_slice(&[0x00, 0x00]).unwrap(); // 附加记录数：0
    response.extend_from_slice(&request[12..]).unwrap(); // 复制查询部分
    response.extend_from_slice(&[0xc0, 0x0c]).unwrap(); // 指针到查询部分
    response.extend_from_slice(&[0x00, 0x01]).unwrap(); // 类型：A
    response.extend_from_slice(&[0x00, 0x01]).unwrap(); // 类别：IN
    response.extend_from_slice(&[0x00, 0x00, 0x00, 0x3c]).unwrap(); // TTL：60秒
    response.extend_from_slice(&[0x00, 0x04]).unwrap(); // 数据长度：4字节
    response.extend_from_slice(&ip.octets()).unwrap(); // IP 地址

    response
}