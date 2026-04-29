//! Broadcast format converters.
//!
//! Each submodule provides a `Convert` type that subscribes to a moq broadcast and
//! republishes it in a different container format. Use this to bridge between hang
//! Legacy and CMAF without going through an external transcoder.

pub mod cmaf;
pub mod hang;
