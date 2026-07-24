//! Encode raw PCM and publish it as a moq audio track.
//!
//! The output codec is selected via [`Codec`].
//!
//! Entry points, high to low level:
//! - `publish_capture` captures a microphone (or system audio) and publishes
//!   it (turnkey). Requires the `capture` feature.
//! - [`Encoder`] encodes raw PCM you supply, and [`Producer`] publishes the
//!   resulting packets (bring your own PCM).
//! - [`Producer`] alone publishes PCM you hand it, encoding as it goes.
//!
//! [`Input`] declares the PCM layout going in. [`Config`] configures the
//! bring-your-own-PCM [`Encoder`], which needs that layout up front; [`Options`]
//! configures [`Producer`] and `publish_capture`, which learn it from the
//! caller's frames or the capture source instead. The decode/consume counterpart
//! lives in the sibling [`decode`](crate::decode) module.
//!
//! `publish_capture` is unlinked above because it only exists with the `capture`
//! feature, so a default-feature rustdoc build has nothing to link to.

mod encoder;
mod producer;

#[cfg(feature = "capture")]
mod capture;

pub use encoder::{Codec, Config, Encoder, Input};
pub use producer::{Options, Producer};

#[cfg(feature = "capture")]
pub use capture::publish_capture;
