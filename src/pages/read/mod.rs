mod page;
pub use page::ReadPage;
pub(crate) use page::PAGE_INDEX;
#[cfg(feature = "ttf_spike")]
pub(crate) use page::READING_MODE;

#[cfg(feature = "epd2in7")]
mod layout_176x264;
#[cfg(feature = "epd2in7")]
pub use layout_176x264::*;

#[cfg(feature = "epd4in2")]
mod layout_400x300;
#[cfg(feature = "epd4in2")]
pub use layout_400x300::*;

#[cfg(feature = "epd2in9")]
mod layout_128x296;
#[cfg(feature = "epd2in9")]
pub use layout_128x296::*;
