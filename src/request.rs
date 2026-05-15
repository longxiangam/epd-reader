use alloc::vec;
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

const BUFFER_SIZE:usize = 4096;
const TLS_BUFFER_SIZE:usize = 4096;
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
    rx_buffer: Vec<u8>,
    tx_buffer: Vec<u8>,
    tls_rx_buffer: Vec<u8>,
    tls_tx_buffer: Vec<u8>,
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
            rx_buffer: vec![0u8;BUFFER_SIZE],
            tx_buffer: vec![0u8;BUFFER_SIZE],
            tls_rx_buffer: vec![0u8;TLS_BUFFER_SIZE],
            tls_tx_buffer: vec![0u8;TLS_BUFFER_SIZE],
        }
    }
    pub async fn send_request(&mut self, url: &str) -> Result<ResponseData, RequestError> {
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
            self.send_https_request(url, host, port, path).await
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
            self.send_plain_http_request(url, host, port, path).await
        } else {
            Err(RequestError::UnsupportedScheme)
        }
    }


    /// Send a plain HTTP request
    async fn send_plain_http_request(
        &mut self,
        url: &str,
        host: &str,
        port: u16,
        path: &str,
    ) -> Result<ResponseData, RequestError> {
        println!("Send plain HTTP request to path {path} at host {host}:{port}");

        let ip_address = self.resolve(host).await?;
        let remote_endpoint = (ip_address, port);

        println!("Create TCP socket");
        let mut socket = TcpSocket::new(*self.stack, &mut self.rx_buffer, &mut self.tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        println!("Connect to HTTP server");
        socket.connect(remote_endpoint).await?;
        println!("Connected to HTTP server");

        let mut request = Request::get(path).host(host).build();

        request.write_header(&mut socket).await?;
        use embedded_io_async::Write;
        let _ = socket.flush().await;


        let mut headers_buf = vec![0u8; 1024];
        let mut buf = vec![0u8; BUFFER_SIZE];
        let response = Response::read(&mut socket, Method::GET, &mut headers_buf).await?;

        println!("Response status: {:?}", response.status);

        let total_length = response.body().reader().read_to_end(&mut buf).await?;

        println!("Close TCP socket");
        socket.close();

        println!("Read {} bytes", total_length);
        return Ok(crate::request::ResponseData{ data: buf, length: total_length });
    }

    /// Send an HTTPS request
    async fn send_https_request(
        &mut self,
        url: &str,
        host: &str,
        port: u16,
        path: &str,
    ) -> Result<ResponseData, RequestError>  {
        println!("Send HTTPs request to path {path} at host {host}:{port}");

        let ip_address = self.resolve(host).await?;
        let remote_endpoint = (ip_address, port);

        let mut socket = TcpSocket::new(*self.stack, &mut self.rx_buffer, &mut self.tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        println!("Connect to HTTP server");
        socket.connect(remote_endpoint).await?;
        println!("Connected to HTTP server");

        let config = TlsConfig::new()
            .with_server_name(host)
            .enable_rsa_signatures();
        let mut tls = TlsConnection::new(
            socket,
            &mut self.tls_rx_buffer,
            &mut self.tls_tx_buffer
            ,
        );

        println!("Perform TLS handshake");
        tls.open(TlsContext::new(&config, UnsecureProvider::new::<Aes128GcmSha256>(&mut self.rng)))
            .await?;
        println!("TLS handshake succeeded");

        let request = Request::get(path).host(host).build();
        request.write_header(&mut tls).await?;
        use embedded_io_async::Write;
        let _ = tls.flush().await;

        let mut headers_buf = vec![0u8; 1024];
        let mut buf = vec![0u8; BUFFER_SIZE];
        let response = Response::read(&mut tls, Method::GET, &mut headers_buf).await?;

        println!("Response status: {:?}", response.status);

        let total_length = response.body().reader().read_to_end(&mut buf).await?;

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

        return Ok(crate::request::ResponseData{ data: buf, length: total_length });
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
