#![no_std]

use core::panic::PanicInfo;
use core::fmt::Write;
use esp_println::println;
use heapless::String;
use crate::storage::{ErrorLogStorage, NvsStorage};
use esp_backtrace::arch;

#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    println!("");
    println!("");
    println!("!! 发生panic错误 !!");
    println!("");

    // 构建错误信息字符串
    let mut error_msg = heapless::String::<200>::new();
    
    // 添加panic位置信息
    if let Some(location) = info.location() {
        let _ = error_msg.push_str("location: ");
        let _ = error_msg.push_str(location.file());
        let _ = error_msg.push_str(":");
        let _ = write!(error_msg, "{}", location.line());
        let _ = error_msg.push_str(":");
        let _ = write!(error_msg, "{}", location.column());
        let _ = error_msg.push_str("\n");
    } else {
        let _ = error_msg.push_str("location: unknow\n");
    }

    // 添加panic消息
    let message = info.message();
    let _ = error_msg.push_str("error: ");
    let _ = write!(error_msg, "{}", message);
    let _ = error_msg.push_str("\n");
    
    // 添加调用栈信息
    let _ = error_msg.push_str("stack:\n");
    let backtrace = arch::backtrace();
    let mut stack_count = 0;
    for addr in backtrace {
        if let Some(addr) = addr {
            if stack_count < 10 { // 限制调用栈深度为10层
                let _ = write!(error_msg, "  #{} 0x{:x}\n", stack_count, addr);
                stack_count += 1;
            } else {
                break;
            }
        }
    }
    if stack_count == 0 {
        let _ = error_msg.push_str("call stack information is not available\n");
    }

    // 限制字符串长度为200字符
    if error_msg.len() > 200 {
        error_msg.truncate(200);
    }

    // 保存错误信息到存储
    let mut error_storage = match ErrorLogStorage::read() {
        Ok(storage) => storage,
        Err(_) => ErrorLogStorage::default(),
    };

    error_storage.error_count += 1;
    
    // 直接保存新错误信息，不保留旧消息
    error_storage.last_error = error_msg;

    // 尝试保存到flash
    if let Err(e) = error_storage.write() {
        println!("保存错误日志失败: {:?}", e);
    } else {
        println!("错误日志已保存到flash");
    }

    // 打印错误信息到控制台
    println!("错误计数: {}", error_storage.error_count);
    println!("最后错误: {}", error_storage.last_error);
    println!("");
    println!("错误已保存，系统将重启...");

    // 保存错误日志到flash
    if let Err(e) = error_storage.write() {
        println!("保存错误日志失败: {:?}", e);
    } else {
        println!("错误日志已保存到flash");
    }

    // 等待一段时间让日志写入完成
    for _ in 0..1000000 {
        core::hint::spin_loop();
    }

    // 重启系统
    esp_hal::reset::software_reset();
    loop{
        
    }
}
