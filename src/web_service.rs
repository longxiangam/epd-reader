use core::str::{from_utf8, FromStr};
use embassy_futures::select::{Either, select};
use embassy_net::{IpListenEndpoint, Stack};
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_println::{print, println};
use esp_wifi::wifi::WifiDevice;
use esp_hal::reset::software_reset;
use heapless::Vec;
use crate::wifi::{AP_STACK_MUT, finish_wifi,  use_wifi, WIFI_MODEL, WifiModel};
// use crate::storage::{NvsStorage, WIFI_INFO};

pub static STOP_WEB_SERVICE: Signal<CriticalSectionRawMutex,()> = Signal::new();
#[embassy_executor::task]
pub async fn web_service(){
    match WIFI_MODEL.lock().await.unwrap() {
        WifiModel::AP => {
            unsafe {
                if let Some(stack) = AP_STACK_MUT {
                    web_tcp_socket(stack).await;
                }
            }
        }
        WifiModel::STA => {
            unsafe {
                loop {
                    match use_wifi().await {
                        Ok(stack) => {
                            web_tcp_socket(stack).await;
                            finish_wifi().await;
                            break;
                        }
                        Err(_) => {}
                    }
                    Timer::after(Duration::from_millis(100));
                }
            }
        }
    }

}

async fn  web_tcp_socket<D: esp_wifi::wifi::WifiDeviceMode> (stack:&Stack<WifiDevice<'_,D>>){

    let mut rx_buffer = [0; 1536];
    let mut tx_buffer = [0; 1536];
    //网页配置服务
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));
    loop {
        println!("Wait for connection...");
        let wait_stop = STOP_WEB_SERVICE.wait();
        let r = socket
            .accept(IpListenEndpoint {
                addr: None,
                port: 80,
            })
            ;
        match select(wait_stop,r).await{
            Either::First(_) => {
                STOP_WEB_SERVICE.reset();
                break;
            }
            Either::Second(r) => {

                println!("Connected...");

                if let Err(e) = r {
                    println!("connect error: {:?}", e);
                    continue;
                }

                use embedded_io_async::Write;

                let mut buffer = [0u8; 2048];
                let mut pos = 0;
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) => {
                            println!("read EOF");
                            break;
                        }
                        Ok(len) => {
                            let to_print =
                                unsafe { core::str::from_utf8_unchecked(&buffer[..(pos + len)]) };

                            if to_print.contains("\r\n\r\n") {
                                print!("{}", to_print);
                                println!();

                                process_http(&mut socket,to_print).await;
                                break;
                            }

                            pos += len;
                        }
                        Err(e) => {
                            println!("read error: {:?}", e);
                            break;
                        }
                    };
                }

                let r = socket.flush().await;
                if let Err(e) = r {
                    println!("flush error: {:?}", e);
                }
                Timer::after(Duration::from_millis(1000)).await;

                socket.close();
                Timer::after(Duration::from_millis(1000)).await;

                socket.abort();

            }
        }
    }

}
async fn process_http(socket:&mut TcpSocket<'_>,buffer:&str) {
    use embedded_io_async::Write;
    use heapless::String;

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(buffer.as_ref());
    println!("request:{:?}", req);
    if let Some("GET") = req.method {
        if let Some("/config") = req.path {
            let content = concat!("HTTP/1.0 200 OK\r\n\r\n", include_str!("../files/config.html"));
            let r = socket
                .write_all(
                    content.as_bytes()
                )
                .await;

            if let Err(e) = r {
                println!("write error: {:?}", e);
            }
        }else{
            //默认
            let content = concat!("HTTP/1.0 200 OK\r\n\r\n", include_str!("../files/config.html"));
            let r = socket
                .write_all(
                    content.as_bytes()
                )
                .await;

            if let Err(e) = r {
                println!("write error: {:?}", e);
            }
        }
    }
    if let Some("POST") = req.method {
        if let Some("/configure_wifi") = req.path {
            let form_fields = parse_form(&req, buffer);
            println!("form_data:{:?}", form_fields);

            if let Ok(fields) = form_fields {
                let mut ssid: Option<&str> = None;
                let mut password: Option<&str> = None;

                for field in fields {
                    if field.0 == "ssid" {
                        ssid = Some(field.1);
                        println!("ssid:{}", field.1);
                    } else if field.0 == "password" {
                        password = Some(field.1);
                        println!("password:{}", field.1);
                    }
                }

                // if let Some(wifi_info) = WIFI_INFO.lock().await.as_mut() {
                //     println!("wifi_info:{:?}", wifi_info);
                //     wifi_info.wifi_ssid = String::from_str(ssid.unwrap()).unwrap();
                //     wifi_info.wifi_password = String::from_str(password.unwrap()).unwrap();
                //     wifi_info.wifi_finish = true;
                //     match wifi_info.write() {
                //         Ok(_) => {
                //             println!("保存成功");
                //             software_reset();
                //         }
                //         Err(e) => {
                //             println!("保存失败：{:?}", e);
                //         }
                //     }
                // }

                let r = socket
                    .write_all(
                        b"HTTP/1.0 200 OK\r\n\r\n\
            <html>\
                <body>\
                   <form action='/restart' method='POST'>\
                    <br/>\
                    <br/>\
                    <input type='submit' value='' />\
                   </form>\
                </body>\
            </html>\r\n",
                    )
                    .await;

                if let Err(e) = r {
                    println!("write error: {:?}", e);
                }
            }
        }
    }
}
fn parse_form<'a>(
    req: &httparse::Request,
    buffer: &'a str,
) -> Result<Vec<(&'a str, &'a str), 20>, &'static str> {
    let (_, body) = buffer.split_once("\r\n\r\n").ok_or("Invalid request format")?;
    let content_type = req
        .headers
        .iter()
        .find(|h| h.name == "Content-Type")
        .ok_or("No Content-Type header found")?;

    let boundary = if content_type.value.starts_with(b"multipart/form-data") {
        let boundary_str = from_utf8(content_type.value).map_err(|_| "Invalid Content-Type header")?;
        boundary_str
            .split(';')
            .find_map(|part| part.trim().strip_prefix("boundary="))
            .ok_or("No boundary found in Content-Type header")?
    } else {
        return Err("Content-Type is not multipart/form-data");
    };
    println!("boundary:{:?}",boundary);

    let mut result: Vec<(&'a str, &'a str), 20> = Vec::new();
    let form_fields: Vec<&str, 20> = body.split(boundary).collect();

    for form_field in form_fields {
        println!("form_field:{:?}",form_field);
        let field = form_field.trim();
        println!("form_field1:{:?}",field);

        if field.contains("Content-Disposition: form-data;") {
            println!("form_field2:{:?}",field);
            if let Some(field) = field.strip_prefix("Content-Disposition: form-data;") {
                println!("form_field3:{:?}",field);
                if let Some((field_name, field_value)) = field.split_once("\r\n\r\n") {
                    println!("form_field4:{:?}",field_name);
                    println!("form_field5:{:?}",field_value);
                    let field_name = field_name
                        .split(';')
                        .find_map(|part| part.trim().strip_prefix("name="))
                        .ok_or("No name attribute found in form-data")?;
                    let field_name = field_name.trim_matches('"');
                    let field_value = field_value.trim_matches('-').trim();

                    result.push((field_name, field_value)).map_err(|_| "Too many form fields")?;
                }
            }
        }

    }

    if result.is_empty() {
        Err("No valid form fields found")
    } else {
        Ok(result)
    }
}

