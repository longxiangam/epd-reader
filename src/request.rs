use alloc::vec::Vec;
use core::num::ParseIntError;
use embassy_net::{IpAddress, Stack};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{ConnectError, TcpSocket};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, TlsError, UnsecureProvider};
use esp_println::println;
use reqwless::Error;
use reqwless::request::{Method, Request, RequestBuilder};
use reqwless::response::Response;
use crate::random::RngWrapper;

// 缓冲区各段尺寸
const TCP_BUF: usize = 4096;
const TLS_BUF: usize = 4096;
const HEADERS_BUF: usize = 1024;
/// 响应体缓冲。新浪 K 线日K(60根)≈6.4KB、分时≈5.8KB；天气/节假日<4KB。
const RESPONSE_BUF: usize = 8 * 1024;

// 请求缓冲放静态 .bss 区（而非每次在堆上 alloc 25KB）。
// SAFETY: WIFI_LOCK（use_wifi/finish_wifi）串行化所有请求，同一时刻只有一个 RequestClient
// 使用这些缓冲；ESP32-C3 单核。因此这里取可变引用不会与其它请求竞争。
static mut RX_BUF: [u8; TCP_BUF] = [0; TCP_BUF];
static mut TX_BUF: [u8; TCP_BUF] = [0; TCP_BUF];
static mut TLS_RX_BUF: [u8; TLS_BUF] = [0; TLS_BUF];
static mut TLS_TX_BUF: [u8; TLS_BUF] = [0; TLS_BUF];
static mut HEADERS_BUF_: [u8; HEADERS_BUF] = [0; HEADERS_BUF];
static mut RESPONSE_BUF_: [u8; RESPONSE_BUF] = [0; RESPONSE_BUF];

#[derive(Debug)]
pub enum RequestError{
    TimeOut,
    UnsupportedScheme,
    PortParse(ParseIntError),
    DnsLookup,
    ConnectError(ConnectError),
    ReqwlessError(reqwless::Error),
    TlsError(TlsError),
    SendError,
    ReadError,
    BufferOver,
}

impl From<ConnectError> for RequestError{
    fn from(value: ConnectError) -> Self {
        RequestError::ConnectError(value)
    }
}
impl From<reqwless::Error> for RequestError{
    fn from(value: Error) -> Self {
       RequestError::ReqwlessError(value)
    }
}

impl From<TlsError> for RequestError {
    fn from(value: TlsError) -> Self {
        RequestError::TlsError(value)
    }
}

pub struct RequestClient{
    stack: &'static Stack<'static>,
    rng: RngWrapper,
}

pub struct ResponseData {
   pub data: Vec<u8>,
   pub length:usize,
}


impl RequestClient{
    pub async fn new(stack: &'static Stack<'static>) -> RequestClient {
        let rng = crate::wifi::HAL_RNG.lock().await.unwrap();
        RequestClient{
            stack,
            rng:RngWrapper::from(rng),
        }
    }

    /// 解析 URL 并把响应读入静态 RESPONSE_BUF，返回读到的字节数（不拷贝）。
    async fn read_to_buf(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<usize, RequestError> {
        if let Some(rest) = url.strip_prefix("https://") {
            println!("Rest: {rest}");
            let (host_and_port, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = if path.is_empty() { "/" } else { path };
            let path = if path.starts_with('/') { path } else { alloc::format!("/{}", path).leak() };
            println!("Host and port: {host_and_port}, path: {path}");
            let (host, port) = host_and_port
                .split_once(':')
                .unwrap_or((host_and_port, "443"));
            println!("Host: {host}, port: {port}, path: {path}");
            let port = port.parse::<u16>().map_err(|e|{ RequestError::PortParse(e)})?;
            self.send_https_request(host, port, path, headers).await
        } else if let Some(rest) = url.strip_prefix("http://") {
            println!("Rest: {rest}");
            let (host_and_port, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = if path.is_empty() { "/" } else { path };
            let path = if path.starts_with('/') { path } else { alloc::format!("/{}", path).leak() };
            println!("Host and port: {host_and_port}, path: {path}");
            let (host, port) = host_and_port
                .split_once(':')
                .unwrap_or((host_and_port, "80"));
            println!("Host: {host}, port: {port}, path: {path}");
            let port = port.parse::<u16>().map_err(|e|{ RequestError::PortParse(e)})?;
            self.send_plain_http_request(host, port, path, headers).await
        } else {
            Err(RequestError::UnsupportedScheme)
        }
    }

    /// 拷贝版：返回拥有所有权的 ResponseData（供响应小、不常请求的调用方使用）。
    #[allow(static_mut_refs)]
    pub async fn send_request(&mut self, url: &str) -> Result<ResponseData, RequestError> {
        let len = self.read_to_buf(url, &[]).await?;
        // SAFETY: WIFI_LOCK 串行化，此处独占 RESPONSE_BUF
        let data = unsafe { RESPONSE_BUF_[..len].to_vec() };
        Ok(crate::request::ResponseData { data, length: len })
    }

    /// 就地版：返回静态 RESPONSE_BUF 的切片，零拷贝。
    /// SAFETY/契约：返回的切片在下次 send_request* 调用前有效；WIFI_LOCK 串行化保证调用方
    /// 会在下一次请求前解析完（解析只读取、把数据搬进自己的结构）。
    #[allow(static_mut_refs)]
    pub async fn send_request_slice(&mut self, url: &str) -> Result<&'static [u8], RequestError> {
        let len = self.read_to_buf(url, &[]).await?;
        // SAFETY: 见上；WIFI_LOCK 保证独占
        Ok(unsafe { &RESPONSE_BUF_[..len] })
    }


    /// 就地带自定义 header（用于需要 Referer 等的请求，如新浪行情）。
    /// SAFETY/契约：同 send_request_slice，返回切片在下次请求前有效。
    #[allow(static_mut_refs)]
    pub async fn send_request_slice_with(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<&'static [u8], RequestError> {
        let len = self.read_to_buf(url, headers).await?;
        // SAFETY: WIFI_LOCK 串行化
        Ok(unsafe { &RESPONSE_BUF_[..len] })
    }


    /// Send a plain HTTP request, 读入 RESPONSE_BUF，返回字节数
    #[allow(static_mut_refs)]
    async fn send_plain_http_request(
        &mut self,
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<usize, RequestError> {
        println!("Send plain HTTP request to path {path} at host {host}:{port}");

        let ip_address = self.resolve(host).await?;
        let remote_endpoint = (ip_address, port);

        // SAFETY: WIFI_LOCK 串行化
        let (rx, tx, resp_hdrs, resp_buf) = unsafe {
            (
                &mut RX_BUF[..],
                &mut TX_BUF[..],
                &mut HEADERS_BUF_[..],
                &mut RESPONSE_BUF_[..],
            )
        };

        println!("Create TCP socket");
        let mut socket = TcpSocket::new(*self.stack, rx, tx);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        println!("Connect to HTTP server");
        socket.connect(remote_endpoint).await?;
        println!("Connected to HTTP server");

        let request = Request::get(path).host(host).headers(headers).build();

        request.write_header(&mut socket).await?;
        let _ = socket.flush().await;

        resp_hdrs.fill(0);
        resp_buf.fill(0);
        let response = Response::read(&mut socket, Method::GET, resp_hdrs).await?;

        println!("Response status: {:?}", response.status);

        let total_length = response.body().reader().read_to_end(resp_buf).await?;

        println!("Close TCP socket");
        socket.close();

        println!("Read {} bytes", total_length);
        Ok(total_length)
    }

    /// Send an HTTPS request, 读入 RESPONSE_BUF，返回字节数
    #[allow(static_mut_refs)]
    async fn send_https_request(
        &mut self,
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<usize, RequestError>  {
        println!("Send HTTPs request to path {path} at host {host}:{port}");

        let ip_address = self.resolve(host).await?;
        let remote_endpoint = (ip_address, port);

        // SAFETY: WIFI_LOCK 串行化
        let (rx, tx, tls_rx, tls_tx, resp_hdrs, resp_buf) = unsafe {
            (
                &mut RX_BUF[..],
                &mut TX_BUF[..],
                &mut TLS_RX_BUF[..],
                &mut TLS_TX_BUF[..],
                &mut HEADERS_BUF_[..],
                &mut RESPONSE_BUF_[..],
            )
        };

        let mut socket = TcpSocket::new(*self.stack, rx, tx);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        println!("Connect to HTTP server");
        socket.connect(remote_endpoint).await?;
        println!("Connected to HTTP server");

        let config = TlsConfig::new()
            .with_server_name(host)
            .enable_rsa_signatures();
        let mut tls = TlsConnection::new(
            socket,
            tls_rx,
            tls_tx,
        );

        println!("Perform TLS handshake");
        tls.open(TlsContext::new(&config, UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng)))
            .await?;
        println!("TLS handshake succeeded");

        let request = Request::get(path).host(host).headers(headers).build();
        request.write_header(&mut tls).await?;
        let _ = tls.flush().await;

        resp_hdrs.fill(0);
        resp_buf.fill(0);
        let response = Response::read(&mut tls, Method::GET, resp_hdrs).await?;

        println!("Response status: {:?}", response.status);

        let total_length = response.body().reader().read_to_end(resp_buf).await?;

        println!("Close TLS wrapper");
        let mut socket = match tls.close().await {
            Ok(socket) => socket,
            Err((socket, error)) => {
                println!("Cannot close TLS wrapper: {error:?}");
                socket
            }
        };

        println!("Close TCP socket");
        socket.close();

        println!("Read {} bytes", total_length);
        Ok(total_length)
    }

    /// Resolve a hostname to an IP address through DNS
    async fn resolve(&mut self, host: &str) -> Result<IpAddress, RequestError> {

        if let  Ok(mut ip_addresses) = self.stack.dns_query(host, DnsQueryType::A).await {
            let ip_address = ip_addresses.pop().ok_or(RequestError::DnsLookup)?;
            println!("Host {host} resolved to {ip_address}");
            Ok(ip_address)
        } else {
           Err(RequestError::DnsLookup)
        }

    }
}
