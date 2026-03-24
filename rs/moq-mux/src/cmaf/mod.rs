mod container;
mod error;

pub use error::*;

pub type Consumer = crate::ordered::Consumer<mp4_atom::Moov>;
pub type Producer = crate::ordered::Producer<mp4_atom::Moov>;
pub use crate::ordered::Frame;
