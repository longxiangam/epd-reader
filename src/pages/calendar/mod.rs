mod page;
pub mod render_data;

pub use page::CalendarPage;

#[cfg(feature = "epd2in7")]
mod layout_264x176;
#[cfg(feature = "epd2in7")]
pub use layout_264x176::{draw, sleep_renderer};

#[cfg(feature = "epd4in2")]
mod layout_400x300;
#[cfg(feature = "epd4in2")]
pub use layout_400x300::{draw, sleep_renderer};

#[cfg(feature = "epd2in9")]
mod layout_128x296;
#[cfg(feature = "epd2in9")]
pub use layout_128x296::{draw, sleep_renderer};
