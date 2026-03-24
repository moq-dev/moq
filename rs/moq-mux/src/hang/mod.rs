mod container;

pub use container::Legacy;

pub type Consumer = crate::ordered::Consumer<Legacy>;
pub type Producer = crate::ordered::Producer<Legacy>;
pub use crate::container::Frame;
