mod container;

pub use container::{Legacy, Media};

pub type Consumer = crate::ordered::Consumer<Media>;
pub type Producer = crate::ordered::Producer<Media>;
pub use crate::container::Frame;
