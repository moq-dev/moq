//! The authorization contract for Media over QUIC.
//!
//! A relay asks one question per session, "may this connect?", and this crate holds
//! every piece of the answer:
//!
//! - [`Request`] and [`Grant`]: the JSON a relay POSTs to an auth server on
//!   [`Event::Connect`], [`Event::Revalidate`], and [`Event::End`], and what comes back.
//! - [`lease::Producer`] / [`lease::Consumer`]: the handle a session holds for its
//!   grant, so whoever runs the accept loop decides how, in process or over HTTP.
//! - [`Client`]: the HTTP implementation that drives a lease against `--auth-url`.
//! - [`serve::Server`]: the reference server behind `moq auth serve`, holding the
//!   policy a relay used to: keys, public rules, an mTLS grant, tiers, and limits.
//! - [`Claims`], [`Key`], and [`KeySet`]: the JWT a client presents in its query, with
//!   the keys that sign and verify it. `moq auth generate|sign|verify` is the CLI.
//!
//! Grants and claims name paths with [`Pattern`]s from `moq-pattern`, re-exported here:
//! `foo` is one broadcast, `foo/**` is a subtree, `**` is everything.

mod algorithm;
mod claims;
mod error;
mod fs;
mod generate;
mod grant;
mod key;
mod key_id;
mod path;
mod request;
mod set;
mod wire;

pub mod lease;
#[cfg(feature = "serve")]
pub mod serve;

#[cfg(feature = "client")]
mod client;

pub use algorithm::*;
pub use claims::*;
#[cfg(feature = "client")]
pub use client::*;
pub use error::*;
pub use grant::*;
pub use key::*;
pub use key_id::*;
pub use moq_pattern::{InvalidPattern, Pattern, Patterns, Segment, Specificity};
pub use request::*;
pub use set::*;
