use core::str::{from_utf8, FromStr};
use embassy_futures::select::{Either, select};
use embassy_net::{IpListenEndpoint, Stack};
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_println::{print, println};
use esp_hal::system::software_reset;
use heapless::Vec;
use crate::wifi::{finish_wifi, use_wifi, WIFI_MODEL, WifiModel};
use crate::sd_mount::{SdMount, SD_MOUNT, url_decode, BOOK_NAME_MAX};
use crate::storage::NvsStorage;

pub static STOP_WEB_SERVICE: Signal<CriticalSectionRawMutex,()> = Signal::new();
#[embassy_executor::task]
pub async fn web_service(){
    match WIFI_MODEL.lock().await.unwrap() {
        WifiModel::AP => {
            unsafe {
                let stack = *core::ptr::addr_of!(crate::wifi::AP_STACK_MUT);
                if let Some(stack) = stack {
                    web_tcp_socket(stack).await;
                }
                Timer::after(Duration::from_millis(100)).await;
            }
        }
        WifiModel::STA => {
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

async fn web_tcp_socket(stack: &'static Stack<'static>){

    let mut rx_buffer = [0; 1536];
    let mut tx_buffer = [0; 1536];
    //网页配置服务
    let mut socket = TcpSocket::new(*stack, &mut rx_buffer, &mut tx_buffer);
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

                    // 1) 读到 HTTP header 结束（\r\n\r\n）
                    let mut header_end = None;
                    while header_end.is_none() {
                        if pos >= buffer.len() {
                            break;
                        }
                        match socket.read(&mut buffer[pos..]).await {
                            Ok(0) => break,
                            Ok(len) => {
                                pos += len;
                                if let Some(i) = find_header_end(&buffer[..pos]) {
                                    header_end = Some(i);
                                }
                            }
                            Err(e) => {
                                println!("read error: {:?}", e);
                                break;
                            }
                        }
                    }
                    let header_end = match header_end {
                        Some(i) => i,
                        None => break, // 没有完整 header，断开重连
                    };

                    // 2) 按 Content-Length 继续读完 body。
                    //    关键修复：之前遇到 \r\n\r\n 就 break，跨多个 TCP 分段的大表单
                    //    （如 5 支股票 10 个字段）后半段 body 读不进来，导致 parse_form 丢字段。
                    let content_length = parse_content_length(&buffer[..header_end]);
                    let want = (header_end + content_length).min(buffer.len());
                    while pos < want {
                        match socket.read(&mut buffer[pos..]).await {
                            Ok(0) => break,
                            Ok(len) => pos += len,
                            Err(_) => break,
                        }
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

/// 从 HTTP header 中解析 Content-Length（不区分大小写）
fn parse_content_length(header: &[u8]) -> usize {
    let s = match core::str::from_utf8(header) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    const PREFIX: &str = "content-length:";
    for line in s.split('\n') {
        let line = line.trim();
        if line.len() >= PREFIX.len() && line[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
            if let Ok(n) = line[PREFIX.len()..].trim().parse::<usize>() {
                return n;
            }
        }
    }
    0
}

/// Send JSON response with HTTP/1.1 keep-alive and Content-Length.
async fn send_json(socket: &mut TcpSocket<'_>, body: &[u8]) {
    use embedded_io_async::Write;
    let header = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(body).await;
}

/// 把字符串以 JSON 转义形式追加（含中文按 UTF-8 原样）
fn json_str(out: &mut alloc::string::String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// 返回当前所有配置（供前端异步回显，与 /books 同样的模式）
async fn handle_get_config(socket: &mut TcpSocket<'_>) {
    let wifi = crate::storage::WifiStorage::read().unwrap_or_default();
    let weather = crate::storage::WeatherStorage::read().unwrap_or_default();
    let sleep = crate::storage::SleepStorage::read().unwrap_or_default();
    let stock = crate::storage::StockStorage::read().unwrap_or_default();

    let mut body = alloc::string::String::new();
    body.push_str("{\"wifi\":{\"ssid\":");
    json_str(&mut body, wifi.wifi_ssid.as_str());
    body.push_str(",\"password\":");
    json_str(&mut body, wifi.wifi_password.as_str());
    body.push_str("},\"weather\":{\"api-key\":");
    json_str(&mut body, weather.token.as_str());
    body.push_str(",\"city\":");
    json_str(&mut body, weather.city.as_str());
    body.push_str("},\"sleep\":{\"read\":");
    body.push_str(&alloc::format!("{}", sleep.read_sleep_seconds));
    body.push_str(",\"weather\":");
    body.push_str(&alloc::format!("{}", sleep.weather_sleep_seconds));
    body.push_str("},\"stocks\":[");
    for i in 0..(stock.count as usize).min(5) {
        if i > 0 { body.push(','); }
        body.push_str("{\"code\":");
        json_str(&mut body, stock.entries[i].code.as_str());
        body.push_str(",\"name\":");
        json_str(&mut body, stock.entries[i].name.as_str());
        body.push('}');
    }
    body.push_str("]}");

    send_json(socket, body.as_bytes()).await;
}

async fn handle_get_images(socket: &mut TcpSocket<'_>) {
    use embedded_io_async::Write;

    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"images\":[],\"error\":\"SD not ready\"}").await;
        return;
    };

    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(e) => {
            println!("open_volume error: {:?}", e);
            send_json(socket, b"{\"images\":[],\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(e) => {
            println!("open_root error: {:?}", e);
            send_json(socket, b"{\"images\":[],\"error\":\"open root failed\"}").await;
            return;
        }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(e) => {
            println!("open_dir images error: {:?}", e);
            send_json(socket, b"{\"images\":[],\"error\":\"open images dir failed\"}").await;
            return;
        }
    };

    let images = SdMount::get_images(&mut images_dir).unwrap_or_default();

    let mut body_len: usize = 11; // {"images":[
    for (i, img) in images.iter().enumerate() {
        if i > 0 { body_len += 1; }
        body_len += 2;
        for c in img.chars() {
            match c {
                '"' | '\\' => body_len += 2,
                '\n' | '\r' => body_len += 2,
                _ => body_len += c.len_utf8(),
            }
        }
    }
    body_len += 2; // ]}

    let header = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body_len
    );
    let _ = socket.write_all(header.as_bytes()).await;
    let _ = socket.write_all(b"{\"images\":[").await;

    for (i, img) in images.iter().enumerate() {
        if i > 0 { let _ = socket.write_all(b",").await; }
        let mut json_name = heapless::String::<{ BOOK_NAME_MAX + 10 }>::new();
        json_name.push('"').ok();
        for c in img.chars() {
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

async fn handle_delete_image(socket: &mut TcpSocket<'_>, _req: &httparse::Request<'_, '_>, header_str: &str) {
    let body = match header_str.split_once("\r\n\r\n") {
        Some((_, b)) => b,
        None => { send_json(socket, b"{\"success\":false}").await; return; }
    };
    let name_encoded = body.strip_prefix("name=").unwrap_or("").trim_end_matches('&');
    let image_name = match url_decode(name_encoded) {
        Some(n) => n,
        None => { send_json(socket, b"{\"success\":false,\"error\":\"invalid name\"}").await; return; }
    };
    println!("delete image: {}", image_name);

    let mut sd_guard = SD_MOUNT.lock().await;
    let Some(ref mut sd) = *sd_guard else {
        send_json(socket, b"{\"success\":false,\"error\":\"SD not ready\"}").await;
        return;
    };
    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await; return; }
    };
    let root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await; return; }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open images dir failed\"}").await; return; }
    };

    let bmp_file_name = alloc::format!("{}.bmp", image_name);
    if let Some(entry) = SdMount::find_entry_by_name(&mut images_dir, &bmp_file_name) {
        let _ = images_dir.delete_file_in_dir(entry.name);
        send_json(socket, b"{\"success\":true}").await;
    } else {
        send_json(socket, b"{\"success\":false,\"error\":\"image not found\"}").await;
    }
}

async fn handle_upload_image(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str, header_data: &[u8]) {
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
        None => { send_json(socket, b"{\"success\":false,\"error\":\"invalid file name\"}").await; return; }
    };
    println!("upload image: {} chunk={} len={}", file_name, chunk_index, content_length);

    let header_end = match header_str.find("\r\n\r\n") {
        Some(pos) => pos + 4,
        None => { send_json(socket, b"{\"success\":false}").await; return; }
    };
    let body_already_read = header_data.len().saturating_sub(header_end);

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

    let mut lfn_short_name: Option<embedded_sdmmc::ShortFileName> = None;
    if chunk_index == 0 && !file_name.is_ascii() {
        match SdMount::create_image_file_with_lfn(&mut sd.volume_manager, &file_name) {
            Ok((short_name, _block, _offset)) => {
                println!("Image LFN created: {} -> {}", file_name, short_name);
                lfn_short_name = Some(short_name);
            }
            Err(_) => {
                send_json(socket, b"{\"success\":false,\"error\":\"create image lfn failed\"}").await;
                return;
            }
        }
    }

    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await; return; }
    };
    let root = match volume0.open_root_dir() {
        Ok(r) => r,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open root failed\"}").await; return; }
    };
    let mut images_dir = match root.open_dir("images") {
        Ok(d) => d,
        Err(_) => { send_json(socket, b"{\"success\":false,\"error\":\"open images dir failed\"}").await; return; }
    };

    let file = if let Some(ref sn) = lfn_short_name {
        match images_dir.open_file_in_dir(sn.clone(), mode) {
            Ok(f) => f,
            Err(e) => {
                println!("open by short name failed: {:?}", e);
                send_json(socket, b"{\"success\":false,\"error\":\"open file failed\"}").await;
                return;
            }
        }
    } else {
        match SdMount::open_file_by_name(&mut images_dir, &file_name, mode) {
            Ok(f) => f,
            Err(_) => {
                send_json(socket, b"{\"success\":false,\"error\":\"open file failed\"}").await;
                return;
            }
        }
    };

    if body_already_read > 0 && content_length > 0 {
        let write_len = body_already_read.min(content_length);
        let _ = file.write(&header_data[header_end..header_end + write_len]);
    }

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
                println!("image upload read error: {:?}", e);
                break;
            }
        }
    }

    file.close();
    drop(images_dir);
    drop(root);
    drop(volume0);
    drop(sd_guard);

    let body = alloc::format!("{{\"success\":true,\"chunk\":{}}}", chunk_index);
    send_json(socket, body.as_bytes()).await;
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
        (Some("GET"), Some("/images")) => {
            handle_get_images(socket).await;
        }
        (Some("POST"), Some("/delete")) => {
            handle_delete(socket, &req, header_str).await;
        }
        (Some("POST"), Some("/delete_image")) => {
            handle_delete_image(socket, &req, header_str).await;
        }
        (Some("POST"), Some(path)) if path.starts_with("/upload?") => {
            handle_upload(socket, &req, header_str, header_data).await;
        }
        (Some("POST"), Some(path)) if path.starts_with("/upload_image?") => {
            handle_upload_image(socket, &req, header_str, header_data).await;
        }
        (Some("GET"), Some("/config")) => {
            handle_get_config(socket).await;
        }
        (Some("GET"), Some("/")) | (Some("GET"), Some("/index.html")) => {
            let html = include_str!("../files/config.html");
            let header = alloc::format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
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
        (Some("POST"), Some("/configure_stock")) => {
            handle_configure_stock(socket, &req, header_str).await;
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

    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(e) => {
            println!("open_volume error: {:?}", e);
            send_json(socket, b"{\"books\":[],\"error\":\"open volume failed\"}").await;
            return;
        }
    };

    let root = match volume0.open_root_dir() {
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

async fn handle_delete(socket: &mut TcpSocket<'_>, _req: &httparse::Request<'_, '_>, header_str: &str) {

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

    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let root = match volume0.open_root_dir() {
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
    let volume0 = match sd.volume_manager.open_volume(embedded_sdmmc::VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            send_json(socket, b"{\"success\":false,\"error\":\"open volume failed\"}").await;
            return;
        }
    };
    let root = match volume0.open_root_dir() {
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
    let file = if let Some(ref sn) = lfn_short_name {
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

        if let Some(wifi_info) = crate::storage::WIFI_INFO.lock().await.as_mut() {
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

async fn handle_configure_stock(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str) {
    let form_fields = parse_form(req, header_str);
    println!("stock form_data:{:?}", form_fields);

    if let Ok(fields) = form_fields {
        // 收集 code0..code4 / name0..name4
        let mut codes: [Option<&str>; 5] = [None; 5];
        let mut names: [Option<&str>; 5] = [None; 5];
        for field in fields {
            let (kind, idx) = if let Some(s) = field.0.strip_prefix("code") {
                ("code", s)
            } else if let Some(s) = field.0.strip_prefix("name") {
                ("name", s)
            } else {
                continue;
            };
            let i: usize = idx.parse().unwrap_or(99);
            if i >= 5 {
                continue;
            }
            match kind {
                "code" => codes[i] = Some(field.1),
                _ => names[i] = Some(field.1),
            }
        }

        let mut stock_storage = crate::storage::StockStorage::read().unwrap_or_default();
        let mut count: u8 = 0;
        for i in 0..5 {
            if let Some(c) = codes[i] {
                if !c.is_empty() {
                    let idx = count as usize;
                    stock_storage.entries[idx].code = heapless::String::from_str(c).unwrap_or_default();
                    stock_storage.entries[idx].name = heapless::String::from_str(names[i].unwrap_or("")).unwrap_or_default();
                    count += 1;
                }
            }
        }
        stock_storage.count = count;
        if stock_storage.selected >= count && count > 0 {
            stock_storage.selected = 0;
        }
        let _ = stock_storage.write();
        println!("股票配置保存: {} 支", count);

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
    match crate::flash_sleep::get_sleep_image_size() {
        Some(size) => {
            let body = alloc::format!("{{\"exists\":true,\"size\":{}}}", size);
            send_json(socket, body.as_bytes()).await;
        }
        None => {
            send_json(socket, b"{\"exists\":false}").await;
        }
    }
}

async fn handle_upload_sleep_image(socket: &mut TcpSocket<'_>, req: &httparse::Request<'_, '_>, header_str: &str, header_data: &[u8]) {
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

    let mut raw_bmp = alloc::vec::Vec::with_capacity(content_length);
    if body_already_read > 0 {
        let len = body_already_read.min(content_length);
        raw_bmp.extend_from_slice(&header_data[header_end..header_end + len]);
    }

    let mut remaining = content_length.saturating_sub(body_already_read);
    let mut buf = [0u8; 512];
    while remaining > 0 {
        let to_read = remaining.min(buf.len());
        match socket.read(&mut buf[..to_read]).await {
            Ok(len) => {
                if len == 0 { break; }
                raw_bmp.extend_from_slice(&buf[..len]);
                remaining -= len;
            }
            Err(e) => {
                println!("sleep image upload read error: {:?}", e);
                break;
            }
        }
    }

    match crate::flash_sleep::save_sleep_image(&raw_bmp) {
        Ok(()) => send_json(socket, b"{\"success\":true}").await,
        Err(e) => {
            let msg = alloc::format!("{{\"success\":false,\"error\":\"{}\"}}", e);
            send_json(socket, msg.as_bytes()).await;
        }
    }
}

async fn handle_delete_sleep_image(socket: &mut TcpSocket<'_>) {
    match crate::flash_sleep::delete_sleep_image() {
        Ok(()) => send_json(socket, b"{\"success\":true}").await,
        Err(e) => {
            let msg = alloc::format!("{{\"success\":false,\"error\":\"{}\"}}", e);
            send_json(socket, msg.as_bytes()).await;
        }
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
