mod container;
mod convert;
mod error;

pub(crate) use container::{decode, parse_timescale};
pub use convert::*;
pub use error::Error;

pub type Consumer = crate::ordered::Consumer<mp4_atom::Moov>;
pub type Producer = crate::ordered::Producer<mp4_atom::Moov>;
pub use crate::container::Frame;
