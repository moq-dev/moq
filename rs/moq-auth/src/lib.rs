//! The authorization contract for Media over QUIC.
//!
//! A relay asks one question per session, "may this connect?", and this crate holds
//! every piece of the answer:
//!
//! - [`Request`], [`Grant`], and [`Refusal`]: the JSON a relay POSTs to an auth server on
//!   [`Event::Connect`], [`Event::Attach`], [`Event::Revalidate`], and [`Event::End`], and what comes back.
//! - [`lease::Producer`] / [`lease::Consumer`]: the handle a session holds for its
//!   grant, so whoever runs the accept loop decides how, in process or over HTTP.
//! - [`Client`]: the HTTP implementation that drives a lease against `--auth-url`.
//! - [`serve::Server`]: the reference server behind `moq auth serve`, holding the
//!   policy a relay used to: keys, public rules, an mTLS grant, tiers, and limits.
//! - [`Claims`], [`Key`], and [`KeySet`]: the JWT a client presents in its query, with
//!   the keys that sign and verify it. `moq auth generate|sign|verify` is the CLI.
//!
//! Grants and claims name paths with [`Prefixes`] from `moq-pattern`, re-exported here:
//! `foo` is `foo` and everything beneath it, and the empty prefix is everything. A grant
//! holds no wildcard: a [`Prefix`] refuses `foo/**`, `**`, and `*`. Until the next two
//! version migrations, a token, key scope, or [`Grant`] is written with each prefix as
//! the subtree pattern releases before 0.22 read, `foo/**` or `**`, and reads one back
//! as its prefix; any other wildcard there is refused.
//! Each role is an [`Access`]: its prefixes, and optionally a bitrate cap, a [`Rate`].
//!
//! The lease `poll_*` methods are always available. Enable the `async` feature for their
//! generated async wrappers (`changed`, `closed`, and `revalidate_requested`) and
//! `lease::Consumer::reported`; `client` and `serve` enable it.

#![warn(missing_docs)]

mod access;
mod algorithm;
mod claims;
#[cfg(test)]
mod corpus;
mod crypto;
mod error;
mod fs;
/// Test support for the `fuzz/` harness: the target bodies and their seeds.
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod generate;
mod grant;
mod key;
mod key_id;
mod path;
mod rate;
mod request;
mod set;
mod token_id;
mod wire;

pub mod lease;
pub mod refusal;
pub use refusal::Refusal;
#[cfg(feature = "serve")]
pub mod serve;
pub mod tier;

#[cfg(feature = "client")]
mod client;

pub use access::*;
pub use algorithm::*;
pub use claims::*;
#[cfg(feature = "client")]
pub use client::*;
pub use error::*;
pub use grant::*;
pub use key::*;
pub use key_id::*;
pub use moq_pattern::{InvalidPattern, InvalidPrefix, Prefix, Prefixes};
pub use rate::*;
pub use request::*;
pub use set::*;
pub use token_id::*;
