//! Headless multi-participant rooms over MoQ.
//!
//! A room is a path prefix. There is no service and no storage: joining is
//! minting a moq-token rooted at that prefix (the LiveKit AccessToken analogue)
//! and dialing the relay.
//!
//! Participants are discovered from the announce stream. Identity is the path
//! before `camera` / `screen`. Each participant publishes:
//!
//! - `{identity}/camera.hang`: camera + microphone
//! - `{identity}/screen.hang`: screenshare; its announce/unannounce is the share lifecycle
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

/// Errors constructing or consuming a room participant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A room track could not be opened or read.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
	/// A chat record could not be encoded or decoded.
	#[error(transparent)]
	Json(#[from] moq_json::Error),
	/// A participant identity must contain a nonempty path.
	#[error("participant identity must not be empty")]
	EmptyIdentity,
}
