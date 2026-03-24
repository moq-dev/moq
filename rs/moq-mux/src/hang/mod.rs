mod container;

pub use container::Legacy;

pub type OrderedConsumer = crate::consumer::OrderedConsumer<Legacy>;
pub type OrderedProducer = crate::producer::OrderedProducer<Legacy>;
pub use crate::frame::{Frame, OrderedFrame};
