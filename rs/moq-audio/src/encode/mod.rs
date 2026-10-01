//! Encode raw PCM and publish it as a moq audio track.
//!
//! The output codec is selected via [`Codec`], and the implementation behind it
//! via [`Kind`]: a platform encoder when the host has one, software otherwise.
//! AAC has no software encoder, so a host without a platform one refuses it.
//!
//! Entry points, high to low level:
//! - `publish_capture` captures a microphone (or system audio) and publishes
//!   it (turnkey). Requires the `capture` feature.
//! - [`Encoder`] encodes raw PCM you supply into [`Encoded`] packets, and
//!   [`Producer`] publishes them (bring your own PCM).
//! - [`Producer`] alone publishes PCM you hand it, encoding as it goes.
//!
//! [`Input`] declares the source PCM while [`Settings`] describes exactly what
//! the codec accepts. [`Options`] combines those settings with publication policy.
//!
//! `publish_capture` is unlinked above because it only exists with the `capture`
//! feature, so a default-feature rustdoc build has nothing to link to.

mod backend;
mod encoded;
mod encoder;
mod producer;

#[cfg(feature = "capture")]
mod capture;

pub use encoded::Encoded;
pub use encoder::{Codec, Encoder, Finish, Input, Kind, Settings};
pub use producer::{Options, Producer};

#[cfg(feature = "capture")]
pub use capture::{Driver, Level, Publication, PublicationOptions, State, Status, publish_capture};
