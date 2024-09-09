use embassy_executor::Spawner;
use crate::pages::Page;

struct ReadMenuPage{
    running:bool,
    need_render:bool,
}

impl Page for ReadMenuPage{
    fn new() -> Self {
        todo!()
    }

    async fn render(&mut self) {
        todo!()
    }

    async fn run(&mut self, spawner: Spawner) {
        todo!()
    }

    async fn bind_event(&mut self) {
        todo!()
    }
}