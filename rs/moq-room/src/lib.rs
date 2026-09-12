//! Headless multi-participant rooms over MoQ.
//!
//! A room is a path prefix. There is no service and no storage: joining is
//! minting a moq-token rooted at that prefix (the LiveKit AccessToken analogue)
//! and dialing the relay.
//!
//! Participants are discovered from the announce stream. Identity is the path
//! before `camera` / `screen`. Each participant publishes:
//!
//! - `{identity}/camera` — camera + microphone
//! - `{identity}/screen` — screenshare; its announce/unannounce is the share lifecycle
//!
//! This is the native counterpart of [`@moq/room`](https://www.npmjs.com/package/@moq/room),
//! extracted from hang.live's roster and iroh-live's announce-bus redesign of
//! `iroh-rooms`. Gossip discovery, tickets, and 1:1 Call stay in iroh-live.
//! Capture and encode stay in `moq-video` / `moq-audio`.

#![warn(missing_docs)]

pub mod chat;
mod claims;
mod path;
mod room;

pub use claims::claims;
pub use path::{Kind, Parsed, broadcast_path, kind_from_segment, parse};
pub use room::{Event, Room};
