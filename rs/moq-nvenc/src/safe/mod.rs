//! Safe wrapper around the raw bindings.
//!
//! Largely unfinished, so you might still have to dip into
//! [`sys`](crate::sys) for the missing functionality.

mod api;
mod buffer;
mod builders;
mod encoder;
mod result;
mod session;

pub use buffer::{Bitstream, BitstreamLock, Buffer, BufferLock, EncoderInput, RegisteredResource};
pub use encoder::{Encoder, EncoderInitParams};
pub use result::{EncodeError, ErrorKind};
pub use session::{EncodePictureParams, Session, Submission};
