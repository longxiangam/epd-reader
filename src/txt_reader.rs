use core::str::FromStr;
use embedded_hal_bus::spi::CriticalSectionDevice;
use embedded_sdmmc::{File, SdCard};
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::peripherals::SPI2;
use esp_hal::spi::FullDuplexMode;
use esp_hal::spi::master::Spi;
use esp_println::println;
use heapless::{String, Vec};
use log::debug;
use reqwless::request::RequestBody;
use crate::TimeSource;

pub struct TxtReader;

type FileObject<'a,'b,CS: esp_hal::gpio::OutputPin> = File<'b,SdCard<&'a mut CriticalSectionDevice<'a,Spi<'a,SPI2, FullDuplexMode>, Output<'a,CS>, Delay>, Delay>, TimeSource, 4, 4, 1>;
impl TxtReader {
    pub fn generate_pages<CS: esp_hal::gpio::OutputPin>(my_file:& mut FileObject<CS>){

        let mut utf8_buf:Vec<u8,2100> = Vec::new();//完整的utf8 缓存
        let mut txt_str:String<8000> = String::new();//保存utf8转换的字符串，大于一定长度后进行分页计算
        let mut begin_position = 0;//txt_str开始字节在文件中的位置
        let mut end_position = 0;//txt_str结束字节在文件中的位置
        let mut all_page_position_vec:Vec<u16,500> = Vec::new();

        const BEGIN_PAGE_LEN:usize = 7000;

        const buffer_len:usize = 2000;

        let mut file_length = my_file.length();
        println!("文件大小：{}", file_length);

        while !my_file.is_eof() {
            let mut buffer = [0u8; buffer_len];
            let num_read = my_file.read(&mut buffer).unwrap();
            debug!("buffer num:{}",num_read);
            debug!("buffer : {:?}",buffer );

            let mut cut_buffer = Self::cut_full_utf8(&buffer,num_read,buffer_len);
            for b in &buffer[0..cut_buffer.len()] {
                utf8_buf.push(*b).unwrap();
            }

            debug!("cut_buffer : {:?}",cut_buffer );

            end_position += utf8_buf.as_slice().len();
            // 检查当前缓冲区中的字节是否形成了有效的UTF-8字符
            if let Ok(s) = String::from_utf8(utf8_buf.clone()) {
                txt_str.push_str(s.as_str());
                // 有效的UTF-8字符，可以打印或处理
                debug!("read 字符：{}", s);
                debug!("字符：{}", txt_str);
                utf8_buf.clear(); // 清空缓冲区，准备下一批字节
            } else {
                debug!("Invalid UTF-8 sequence");
                utf8_buf.clear();
            }
            if cut_buffer.len() != num_read {
                for b in &buffer[cut_buffer.len()..num_read] {
                    utf8_buf.push(*b);
                }
            }

            if(txt_str.len() > BEGIN_PAGE_LEN){
                let (lost_str,pages) =  Self::compute_pages(txt_str.as_str(),begin_position);

                //结束位置减掉剩余的长度是新的开始位置，剩余的字符串会重新加入到txt_str开始位置
                begin_position = end_position - lost_str.len();
                txt_str =String::from_str(lost_str).expect("lost_str error");

                //计算进度
                let percent =  begin_position as f32 / file_length as f32 * 100.0;
                println!("完成：{}%", percent);


                all_page_position_vec.extend_from_slice(&pages);

            }

        }

        //结束时最后计算
        let (lost_str,pages) =  Self::compute_pages(txt_str.as_str(),begin_position);
       /* let (lost_str,pages) = crate::epd2in9_txt::compute_page(txt_str.as_str(),begin_position);*/
        all_page_position_vec.extend_from_slice(&pages);

        debug!("txt_str:{}",txt_str);
        debug!("pages:{:?}",all_page_position_vec);


        for i in 0..all_page_position_vec.len() {
            let (start_position,end_position) = Self::get_page_content(i+1,&all_page_position_vec);

            my_file.seek_from_start(start_position as u32);

            let mut buffer = [0u8; BEGIN_PAGE_LEN];
            let num_read = my_file.read(&mut buffer).unwrap();

            let len = end_position - start_position ;
            let len = len as usize;
            let vec:Vec<u8,500> = Vec::from_slice(&buffer[0..len]).expect("REASON");
            if let Ok(screen_txt) = String::from_utf8(vec) {
                println!("page : {} screen_txt:{}",(i+1),screen_txt);
            }

        }

    }

    pub fn generate_pages_nostring<CS: esp_hal::gpio::OutputPin>(my_file:& mut FileObject<CS>){

        const BEGIN_PAGE_LEN:usize = 18000;
        const TXT_STR_LEN:usize = 20100;


        let mut utf8_buf:Vec<u8,2100> = Vec::new();//完整的utf8 缓存
        let mut txt_str:Vec<u8,TXT_STR_LEN> = Vec::new();//保存utf8转换的字符串，大于一定长度后进行分页计算
        let mut begin_position = 0;//txt_str开始字节在文件中的位置
        let mut end_position = 0;//txt_str结束字节在文件中的位置
        let mut all_page_position_vec:Vec<u16,500> = Vec::new();



        const buffer_len:usize = 2000;

        let mut file_length = my_file.length();
        println!("文件大小：{}", file_length);

        while !my_file.is_eof() {
            let mut buffer = [0u8; buffer_len];
            let num_read = my_file.read(&mut buffer).unwrap();
            debug!("buffer num:{}",num_read);
            debug!("buffer : {:?}",buffer );

            let mut cut_buffer = Self::cut_full_utf8(&buffer,num_read,buffer_len);
            utf8_buf.extend_from_slice(&buffer[0..cut_buffer.len()] );


            debug!("cut_buffer : {:?}",cut_buffer );

            end_position += utf8_buf.as_slice().len();
            txt_str.extend_from_slice(&utf8_buf);

            utf8_buf.clear();
            if cut_buffer.len() != num_read {
                utf8_buf.extend_from_slice(&buffer[cut_buffer.len()..num_read]  );
            }
            println!("txt_str：{}", txt_str.len());
            if(txt_str.len() > BEGIN_PAGE_LEN){
                let (lost_str,pages) =  Self::compute_pages_nostring(&txt_str,begin_position);

                //结束位置减掉剩余的长度是新的开始位置，剩余的字符串会重新加入到txt_str开始位置
                begin_position = end_position - lost_str.len();
                txt_str =Vec::from_slice(&lost_str).unwrap();

                //计算进度
                let percent =  begin_position as f32 / file_length as f32 * 100.0;
                println!("完成：{}%", percent);


                all_page_position_vec.extend_from_slice(&pages);

            }

        }

        //结束时最后计算
        let (lost_str,pages) =  Self::compute_pages_nostring(&txt_str,begin_position);
        all_page_position_vec.extend_from_slice(&pages);


        println!("pages:{:?}",all_page_position_vec);


        for i in 0..all_page_position_vec.len() {
            let (start_position,end_position) = Self::get_page_content(i+1,&all_page_position_vec);

            my_file.seek_from_start(start_position as u32);

            let mut buffer = [0u8; 500];
            let num_read = my_file.read(&mut buffer).unwrap();

            let len = end_position - start_position ;
            let len = len as usize;
            let vec:Vec<u8,500> = Vec::from_slice(&buffer[0..len]).expect("REASON");
            if let Ok(screen_txt) = String::from_utf8(vec) {
                println!("page : {} screen_txt:{}",(i+1),screen_txt);
            }else{
                println!("uft8 error");
            }

        }


    }

    //从buffer 中找到utf8可以完整结束的位置并返回
    fn cut_full_utf8(buffer:&[u8],len:usize,full_len:usize)->&[u8]{
        if len < full_len{
            return &buffer[0..len];
        }else {
            let mut tail_position = len-1;

            while   tail_position > 0{
                let last_byte = buffer[tail_position];

                //首位为0 ，ascii
                if last_byte & 0b1000_0000 == 0 {
                    return &buffer[0..=tail_position];
                }
                //是否为字符第一个byte，0b10开头不是第一个byte
                if last_byte & 0b1100_0000 == 0b1000_0000  {
                    tail_position -= 1;
                }else{
                    break;
                }
            }
            if tail_position < 0 {
                return &buffer[0..=0usize];
            }

            &buffer[0..tail_position]
        }
    }

    fn ceil_char_boundary(buffer:&[u8],begin_index:usize)->usize{
        let mut position = begin_index;
        while  position < buffer.len() {
            let byte = buffer[position];

            //首位为0 ，ascii
            if byte & 0b1000_0000 == 0 {
                return position;
            }
            //是否为字符第一个byte，0b10开头不是第一个byte
            if byte & 0b1100_0000 == 0b1000_0000  {
                position += 1;
            }else{
                return position;
            }
        }

        buffer.len()
    }

    fn compute_pages(txt_str:&str,begin_position:usize)->(&str,Vec<u16,100>){

        //position 是对应文件中的下标
        let mut real_position = begin_position as u16;
        let mut page_positions:Vec<u16,100> = Vec::new();


        //index 对应切片的下标
        let mut begin_index:usize = 0;
        while begin_index  < txt_str.len()  {
            let (screen_str, is_full_screen) = Self::compute_page(&txt_str[begin_index..]);

            real_position = real_position + screen_str.len() as u16;
            begin_index = begin_index +  screen_str.len() ;
            page_positions.push(real_position).expect("compute_pages error");

            if !is_full_screen {
                break ;
            }
        }


        (&txt_str[begin_index as usize..],page_positions)


    }

    //计算整屏的文本，返回字符串切片，及是否为完整一屏
    fn compute_page(txt_str:&str)->(&str,bool){
        const LOW_WORD:usize = 30;//起步的字符数量
        if txt_str.len() > LOW_WORD {

            let mut end = txt_str.ceil_char_boundary(LOW_WORD);

            let mut is_full_screen = true;
            //循环判断
            while  end < txt_str.len() {
                if Self::check_full_screen(&txt_str[0..end]) {
                    is_full_screen = true;
                    break;
                }else{
                    is_full_screen = false;
                }
                end+=1;
            }
            (&txt_str[0..end],is_full_screen)
        }else{
            (txt_str,false)
        }
    }

    fn compute_pages_nostring(txt_str:&[u8],begin_position:usize)->(&[u8],Vec<u16,100>){

        //position 是对应文件中的下标
        let mut real_position = begin_position as u16;
        let mut page_positions:Vec<u16,100> = Vec::new();


        //index 对应切片的下标
        let mut begin_index:usize = 0;
        while begin_index  < txt_str.len()  {
            let (screen_str, is_full_screen) = Self::compute_page_nostring(&txt_str[begin_index..]);

            real_position = real_position + screen_str.len() as u16;
            begin_index = begin_index +  screen_str.len() ;
            page_positions.push(real_position).expect("compute_pages error");

            if !is_full_screen {
                break ;
            }
        }


        (&txt_str[begin_index as usize..],page_positions)


    }

    //计算整屏的文本，返回字符串切片，及是否为完整一屏
    fn compute_page_nostring(txt_str:&[u8])->(&[u8],bool){
        const LOW_WORD:usize = 300;//起步的字符数量
        if txt_str.len() > LOW_WORD {

            let mut end = Self::ceil_char_boundary(txt_str,LOW_WORD);

            let mut is_full_screen = true;
            //循环判断
            while  end < txt_str.len() {
                if Self::check_full_screen_nostring(&txt_str[0..end]) {
                    is_full_screen = true;
                    break;
                }else{
                    is_full_screen = false;
                }
                end+=1;
            }
            (&txt_str[0..end],is_full_screen)
        }else{
            (txt_str,false)
        }
    }

    fn check_full_screen(txt_str:&str)->bool{
        true
    }

    fn check_full_screen_nostring(txt_str:&[u8])->bool{
        true
    }
    //从1开始
    fn get_page_content( page_num:usize,pages_vec:&Vec<u16,500>)-> (u16,u16){


        let mut  start_position = 0;
        let mut  end_position = 0;

        let page = page_num - 1;

        if page_num <= pages_vec.len() {
            if page == 0 {
                start_position = 0;
                end_position = pages_vec[page];
            }else{
                start_position = pages_vec[page-1];
                end_position = pages_vec[page];
            }
        }

        println!("start:{},end:{}",start_position,end_position);
        (start_position,end_position)
    }



}




