#[derive(Debug, Default)]
enum FinishType {
    #[default]
    Success,
    Fail,
    Abort,
}

#[derive(Debug, Default)]
enum WorkItem{

    #[default]
    Learn = 1,
    Eat,
    Write,
    Read,
    WatchTv,
    PlayGame,
    UsePhone,

}

#[derive(Debug,Default)]
pub struct TimerLog{
    is_sync:bool,
    finish_type:FinishType,
    begin_timestamp:u64,
    end_timestamp:u64,
    interval:u64,
    work_type:WorkItem
}