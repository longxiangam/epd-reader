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
use crate::sd_mount::{SdMount, SD_MOUNT, url_decode, BOOK_NAME_MAX};
// use crate::storage::{NvsStorage, WIFI_INFO};
use crate::storage::NvsStorage;

pub static STOP_WEB_SERVICE: Signal<CriticalSectionRawMutex,()> = Signal::new();
#[embassy_executor::task]
pub async fn web_service(){
    match WIFI_MODEL.lock().await.unwrap() {
        WifiModel::AP => {
            unsafe {
                if let Some(stack) = AP_STACK_MUT {
                    web_tcp_socket(stack).await;
                }
                Timer::after(Duration::from_millis(100)).await;
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
                    Timer::after(Duration::from_millis(100)).await;
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
                println!("STOP_WEB_SERVICE...");
                break;
            }
            Either::Second(r) => {

                println!("Connected...");

                if let Err(e) = r {
                    println!("connect error: {:?}", e);
                    continue;
                }

                // Keep-alive: process multiple HTTP requests on the same TCP connection
                loop {
                    let mut buffer = [0u8; 2048];
                    let mut pos = 0;
                    let mut header_found = false;

                    loop {
                        match socket.read(&mut buffer[pos..]).await {
                            Ok(0) => {
                                println!("read EOF");
                                break;
                            }
                            Ok(len) => {
                                pos += len;
                                for i in 0..pos.saturating_sub(3) {
                                    if buffer[i] == b'\r' && buffer[i+1] == b'\n'
                                        && buffer[i+2] == b'\r' && buffer[i+3] == b'\n' {
                                        header_found = true;
                                        break;
                                    }
                                }
                                if header_found { break; }
                            }
                            Err(e) => {
                                println!("read error: {:?}", e);
                                break;
                            }
                        };
                    }

                    if !header_found {
                        break; // Connection closed or error, accept new connection
                    }

                    let to_print = unsafe { core::str::from_utf8_unchecked(&buffer[..pos]) };
                    print!("{}", to_print);
                    println!();

                    process_http(&mut socket, &buffer[..pos]).await;
                    // Loop back to read next request on same connection (keep-alive)
                }

                socket.close();
                Timer::after(Duration::from_millis(1)).await;
                socket.abort();

            }
        }
    }

}

fn find_header_end(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if data[i] == b'\r' && data[i + 1] == b'\n' && data[i + 2] == b'\r' && data[i + 3] == b'\n' {
            return Some(i + 4);
        }
    }
    None
}

/// Send JSON response with HTTP/1.1 keep-alive and Content-Length.
async fn send_json(socket: &mut TcpSocket<'_>, body: &[u8]) {
    use embedded_io_async::Write;
    let header = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(body).await;
}

async fn process_http(socket:&mut TcpSocket<'_>, header_data: &[u8]) {
    use embedded_io_async::Write;

    let header_end = match find_header_end(header_data) {
        Some(pos) => pos,
        None => return,
    };

    // Body may contain binary data (file uploads), so validate only header if full check fails
    let header_str = match from_utf8(header_data) {
        Ok(s) => s,
        Err(_) => match from_utf8(&header_data[..header_end]) {
            Ok(s) => s,
            Err(_) => return,
        },
    };

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    if let Err(_) = req.parse(header_str.as_bytes()) {
        return;
    }
    println!("request:{:?}", req);

    match (req.method, req.path) {
        (Some("GET"), Some("/books")) => {
            handle_get_books(socket).await;
        }
        (Some("POST"), Some("/delete")) => {
            handle_delete(socket, &req, header_str).await;
        }
        (Some("POST"), Some(path)) if path.starts_with("/upload?") => {
            handle_upload(socket, &req, header_str, header_data).await;
        }
        (Some("GET"), Some("/")) | (Some("GET"), Some("/index.html")) => {
            let html = include_str!("../files/config.html");
            let header = alloc::format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                html.as_bytes().len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(html.as_bytes()).await;
        }
        (Some("POST"), Some("/configure_wifi")) => {
            handle_configure_wifi(socket, &req, header_str).await;
        }
        (Some("POST"), Some("/configure_weather")) => {
            handle_configure_weather(socket, &req, header_str).await;
        }
        (Some("POST"), Some("/configure_sleep")) => {
            handle_configure_sleep(socket, &req, header_str).await;
        }
        (Some("GET"), Some("/sleep_image")) => {
            handle_get_sleep_image(socket).await;
        }
        (Some("POST"), Some("/upload_sleep_image")) => {
            handle_upload_sleep_image(socket, &req, header_str, header_data).await;
        }
        (Some("POST"), Some("/delete_sleep_image")) => {
            handle_delete_sleep_image(socket).await;
        }
        _ => {
            send_json(socket, b"404 Not Found").await;
        }
    }
}

async fn handle_get_books(socket: &mut TcpSocket<'_>) {
    use embedded_io_async::Write;

    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"books\":[],\"error\":\"SD not ready\"}").await;
        return;
    };

    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(e) => {
            println!("open_volume error: {:?}", e);
            send_json(socket, b"{\"books\":[],\"error\":\"open volume failed\"}").await;
            return;
        }
    };

    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(e) => {
            println!("open_root error: {:?}", e);
            send_json(socket, b"{\"books\":[],\"error\":\"open root failed\"}").await;
            return;
        }
    };

    let mut books_dir = match root.open_dir("books") {
        Ok(d) => d,
        Err(e) => {
            println!("open_dir books error: {:?}", e);
            send_json(socket, b"{\"books\":[],\"error\":\"open books dir failed\"}").await;
            return;
        }
    };

    let books = SdMount::get_books(&mut books_dir).unwrap_or_default();

    // Compute total body length for Content-Length header
    let mut body_len: usize = 10; // {"books":[
    for (i, book) in books.iter().enumerate() {
        if i > 0 { body_len += 1; } // comma
        body_len += 2; // quotes
        for c in book.chars() {
            match c {
                '"' | '\\' => body_len += 2,
                '\n' | '\r' => body_len += 2,
                _ => body_len += c.len_utf8(),
            }
        }
    }
    body_len += 2; // ]}

    // Send header with pre-computed Content-Length
    let header = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body_len
    );
    let _ = socket.write_all(header.as_bytes()).await;

    // Stream body
    let _ = socket.write_all(b"{\"books\":[").await;

    for (i, book) in books.iter().enumerate() {
        if i > 0 {
            let _ = socket.write_all(b",").await;
        }
        // Escape basic JSON special chars in book name
        let mut json_name = heapless::String::<{ BOOK_NAME_MAX + 10 }>::new();
        json_name.push('"').ok();
        for c in book.chars() {
            match c {
                '"' => { json_name.push_str("\\\"").ok(); }
                '\\' => { json_name.push_str("\\\\").ok(); }
                '\n' => { json_name.push_str("\\n").ok(); }
                '\r' => { json_name.push_str("\\r").ok(); }
                _ => { json_name.push(c).ok(); }
            }
        }
        json_name.push('"').ok();
        let _ = socket.write_all(json_name.as_bytes()).await;
    }

    let _ = socket.write_all(b"]}").await;
}

async fn handle_delete(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str) {

    // Extract body
    let body = match header_str.split_once("\r\n\r\n") {
        Some((_, b)) => b,
        None => {
            send_json(socket, b"{\"success\":false}").await;
            return;
        }
    };

    // Parse name=ENCODED_NAME from body
    let name_encoded = body.strip_prefix("name=").unwrap_or("").trim_end_matches('&');
    let book_name = match url_decode(name_encoded) {
        Some(n) => n,
        None => {
            send_json(socket, b"{\"success\":false,\"error\":\"invalid name\"}").await;
            return;
        }
    };
    println!("delete book: {}", book_name);

    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"success\":false,\"error\":\"SD not ready\"}").await;
        return;
    };

    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut books_dir = match root.open_dir("books") {
        Ok(d) => d,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open books dir failed\"}").await;
            return;
        }
    };

    // Find the txt file to get short name, then delete all related files
    let txt_file_name = alloc::format!("{}.txt", book_name);
    if let Some(entry) = SdMount::find_entry_by_name(&mut books_dir, &txt_file_name) {
        let short_name = entry.name;
        // Delete .txt
        let _ = books_dir.delete_file_in_dir(short_name.clone());
        // Delete .idx if exists
        let _ = SdMount::delete_idx_file(&mut books_dir, &short_name);
        // Delete .log if exists
        if let Some(log_name) = SdMount::derive_short_name(&short_name, "LOG") {
            let _ = books_dir.delete_file_in_dir(log_name);
        }
        send_json(socket, b"{\"success\":true}").await;
    } else {
        send_json(socket, b"{\"success\":false,\"error\":\"book not found\"}").await;
    }
}

async fn handle_upload(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str, header_data: &[u8]) {

    // Parse query params from URL: /upload?name=ENCODED&chunk=N
    let path = req.path.unwrap_or("");
    let (mut file_name_encoded, mut chunk_index_str) = ("", "0");
    if let Some(query) = path.split('?').nth(1) {
        for param in query.split('&') {
            if let Some(val) = param.strip_prefix("name=") {
                file_name_encoded = val;
            } else if let Some(val) = param.strip_prefix("chunk=") {
                chunk_index_str = val;
            }
        }
    }

    let mut content_length: usize = 0;
    for h in req.headers.iter() {
        if h.name.eq_ignore_ascii_case("Content-Length") {
            if let Ok(s) = from_utf8(h.value) {
                content_length = s.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }

    if file_name_encoded.is_empty() {
        send_json(socket, b"{\"success\":false,\"error\":\"missing name\"}").await;
        return;
    }

    let chunk_index: u32 = chunk_index_str.trim().parse().unwrap_or(0);

    let file_name = match url_decode(file_name_encoded) {
        Some(n) => n,
        None => {
            send_json(socket, b"{\"success\":false,\"error\":\"invalid file name\"}").await;
            return;
        }
    };
    println!("upload: {} chunk={} len={}", file_name, chunk_index, content_length);

    // Calculate body offset in header_data
    let header_end = match header_str.find("\r\n\r\n") {
        Some(pos) => pos + 4,
        None => {
            send_json(socket, b"{\"success\":false}").await;
            return;
        }
    };
    let body_already_read = header_data.len().saturating_sub(header_end);

    // Lock SD
    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"success\":false,\"error\":\"SD not ready\"}").await;
        return;
    };

    let mode = if chunk_index == 0 {
        embedded_sdmmc::Mode::ReadWriteCreateOrTruncate
    } else {
        embedded_sdmmc::Mode::ReadWriteCreateOrAppend
    };

    // Step 1: For chunk 0 with non-ASCII name, create file with LFN first
    let mut lfn_short_name: Option<embedded_sdmmc::ShortFileName> = None;
    if chunk_index == 0 && !file_name.is_ascii() {
        match SdMount::create_file_with_lfn(&mut sd.volume_manager, &file_name) {
            Ok((short_name, _block, _offset)) => {
                println!("LFN created: {} -> {}", file_name, short_name);
                lfn_short_name = Some(short_name);
            }
            Err(_) => {
                send_json(socket, b"{\"success\":false,\"error\":\"create lfn failed\"}").await;
                return;
            }
        }
    }

    // Step 2: Open volume/root/books_dir and get file handle
    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut books_dir = match root.open_dir("books") {
        Ok(d) => d,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open books dir failed\"}").await;
            return;
        }
    };

    // Open file — use short name directly if LFN was just created
    let mut file = if let Some(ref sn) = lfn_short_name {
        match books_dir.open_file_in_dir(sn.clone(), mode) {
            Ok(f) => f,
            Err(e) => {
                println!("open by short name failed: {:?}", e);
                send_json(socket, b"{\"success\":false,\"error\":\"open file failed\"}").await;
                return;
            }
        }
    } else {
        match SdMount::open_file_by_name(&mut books_dir, &file_name, mode) {
            Ok(f) => f,
            Err(_) => {
                send_json(socket, b"{\"success\":false,\"error\":\"open file failed\"}").await;
                return;
            }
        }
    };

    // Write body data already read with header
    if body_already_read > 0 && content_length > 0 {
        let write_len = body_already_read.min(content_length);
        let _ = file.write(&header_data[header_end..header_end + write_len]);
    }

    // Read remaining body from socket
    let mut remaining = content_length.saturating_sub(body_already_read);
    let mut body_buf = [0u8; 512];
    while remaining > 0 {
        let to_read = remaining.min(body_buf.len());
        match socket.read(&mut body_buf[..to_read]).await {
            Ok(len) => {
                if len == 0 { break; }
                let _ = file.write(&body_buf[..len]);
                remaining -= len;
            }
            Err(e) => {
                println!("upload read error: {:?}", e);
                break;
            }
        }
    }

    file.close();
    drop(books_dir);
    drop(root);
    drop(volume0);
    drop(sd_guard);

    let body = alloc::format!("{{\"success\":true,\"chunk\":{}}}", chunk_index);
    send_json(socket, body.as_bytes()).await;
}

async fn handle_configure_wifi(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str) {
    use heapless::String;

    let form_fields = parse_form(req, header_str);
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

        if let Some(mut wifi_info) = crate::storage::WIFI_INFO.lock().await.as_mut() {
            println!("wifi_info:{:?}", wifi_info);
            wifi_info.wifi_ssid = String::from_str(ssid.unwrap()).unwrap();
            wifi_info.wifi_password = String::from_str(password.unwrap()).unwrap();
            wifi_info.wifi_finish = true;
            match wifi_info.write() {
                Ok(_) => {
                    println!("保存成功");
                    send_json(socket, b"{\"success\":true}").await;
                    Timer::after(Duration::from_millis(100)).await;
                    software_reset();
                }
                Err(e) => {
                    println!("保存失败：{:?}", e);
                    send_json(socket, b"{\"success\":false}").await;
                }
            }
        }
    }
}

async fn handle_configure_weather(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str) {

    let form_fields = parse_form(req, header_str);
    println!("weather form_data:{:?}", form_fields);

    if let Ok(fields) = form_fields {
        let mut api_key: Option<&str> = None;
        let mut location: Option<&str> = None;

        for field in fields {
            if field.0 == "api-key" {
                api_key = Some(field.1);
                println!("api-key:{}", field.1);
            } else if field.0 == "location" {
                location = Some(field.1);
                println!("location:{}", field.1);
            }
        }

        if let (Some(key), Some(city)) = (api_key, location) {
            let mut weather_storage = crate::storage::WeatherStorage::read().unwrap_or_default();
            weather_storage.token = heapless::String::from_str(key).unwrap();
            weather_storage.city = heapless::String::from_str(city).unwrap();

            match weather_storage.write() {
                Ok(_) => {
                    println!("天气配置保存成功");
                    crate::storage::WEATHER_API.lock().await.replace(weather_storage);
                }
                Err(e) => {
                    println!("天气配置保存失败：{:?}", e);
                }
            }
        }

        send_json(socket, b"{\"success\":true}").await;
    }
}

async fn handle_configure_sleep(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str) {

    let form_fields = parse_form(req, header_str);
    println!("sleep form_data:{:?}", form_fields);

    if let Ok(fields) = form_fields {
        let mut read_sleep_seconds: Option<&str> = None;
        let mut weather_sleep_seconds: Option<&str> = None;

        for field in fields {
            if field.0 == "read-sleep-seconds" {
                read_sleep_seconds = Some(field.1);
                println!("read-sleep-seconds:{}", field.1);
            } else if field.0 == "weather-sleep-seconds" {
                weather_sleep_seconds = Some(field.1);
                println!("weather-sleep-seconds:{}", field.1);
            }
        }

        if let (Some(read_sec), Some(weather_sec)) = (read_sleep_seconds, weather_sleep_seconds) {
            if let (Ok(read_val), Ok(weather_val)) = (read_sec.parse::<u64>(), weather_sec.parse::<u64>()) {
                let mut sleep_storage = crate::storage::SleepStorage::read().unwrap_or_default();
                sleep_storage.read_sleep_seconds = read_val;
                sleep_storage.weather_sleep_seconds = weather_val;

                match sleep_storage.write() {
                    Ok(_) => {
                        println!("睡眠配置保存成功");
                    }
                    Err(e) => {
                        println!("睡眠配置保存失败：{:?}", e);
                    }
                }
            }
        }

        send_json(socket, b"{\"success\":true}").await;
    }
}

async fn handle_get_sleep_image(socket: &mut TcpSocket<'_>) {
    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"exists\":false,\"error\":\"SD not ready\"}").await;
        return;
    };

    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"exists\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            send_json(socket, b"{\"exists\":false,\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => {
            send_json(socket, b"{\"exists\":false}").await;
            return;
        }
    };

    if let Some(entry) = SdMount::find_entry_by_name(&mut images_dir, "sleep.bmp") {
        let size = entry.size;
        let body = alloc::format!("{{\"exists\":true,\"size\":{}}}", size);
        send_json(socket, body.as_bytes()).await;
    } else {
        send_json(socket, b"{\"exists\":false}").await;
    }
}

async fn handle_upload_sleep_image(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str, header_data: &[u8]) {
    use embedded_io_async::Write;

    let mut content_length: usize = 0;
    for h in req.headers.iter() {
        if h.name.eq_ignore_ascii_case("Content-Length") {
            if let Ok(s) = from_utf8(h.value) {
                content_length = s.trim().parse::<usize>().unwrap_or(0);
            }
        }
    }

    if content_length == 0 {
        send_json(socket, b"{\"success\":false,\"error\":\"empty body\"}").await;
        return;
    }
    if content_length > 30000 {
        send_json(socket, b"{\"success\":false,\"error\":\"file too large (max 30KB)\"}").await;
        return;
    }

    let header_end = match header_str.find("\r\n\r\n") {
        Some(pos) => pos + 4,
        None => {
            send_json(socket, b"{\"success\":false}").await;
            return;
        }
    };
    let body_already_read = header_data.len().saturating_sub(header_end);

    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"success\":false,\"error\":\"SD not ready\"}").await;
        return;
    };

    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open images dir failed\"}").await;
            return;
        }
    };

    let mut file = match SdMount::open_file_by_name(&mut images_dir, "sleep.bmp", embedded_sdmmc::Mode::ReadWriteCreateOrTruncate) {
        Ok(f) => f,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"create file failed\"}").await;
            return;
        }
    };

    // Write body data already read with header
    if body_already_read > 0 {
        let write_len = body_already_read.min(content_length);
        let _ = file.write(&header_data[header_end..header_end + write_len]);
    }

    // Read remaining body from socket
    let mut remaining = content_length.saturating_sub(body_already_read);
    let mut body_buf = [0u8; 512];
    while remaining > 0 {
        let to_read = remaining.min(body_buf.len());
        match socket.read(&mut body_buf[..to_read]).await {
            Ok(len) => {
                if len == 0 { break; }
                let _ = file.write(&body_buf[..len]);
                remaining -= len;
            }
            Err(e) => {
                println!("sleep image upload read error: {:?}", e);
                break;
            }
        }
    }

    file.close();
    drop(images_dir);
    drop(root);
    drop(volume0);
    drop(sd_guard);

    send_json(socket, b"{\"success\":true}").await;
}

async fn handle_delete_sleep_image(socket: &mut TcpSocket<'_>) {
    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"success\":false,\"error\":\"SD not ready\"}").await;
        return;
    };

    let mut volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let mut root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"images dir not found\"}").await;
            return;
        }
    };

    if let Some(entry) = SdMount::find_entry_by_name(&mut images_dir, "sleep.bmp") {
        match images_dir.delete_file_in_dir(entry.name) {
            Ok(_) => send_json(socket, b"{\"success\":true}").await,
            Err(_) => send_json(socket, b"{\"success\":false,\"error\":\"delete failed\"}").await,
        }
    } else {
        send_json(socket, b"{\"success\":false,\"error\":\"sleep image not found\"}").await;
    }
}

fn parse_form<'a>(
    req: &httparse::Request<'_, '_>,
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
