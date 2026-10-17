//! The ICE side of a str0m peer connection: one UDP socket, its candidates, and
//! the loop that drives an [`str0m::Rtc`] over it.
//!
//! str0m is sans-IO, so [`Agent::next`] alternates between feeding it datagrams
//! and timeouts and draining what it produces (transmits, events, the next
//! timeout), and runs a STUN Binding client on the same socket so the agent can
//! offer server-reflexive candidates. Shared by `moq-tokio`'s data channel
//! transport and `moq-rtc`'s WHIP/WHEP gateway, which only differ in what they do
//! with the events.
//!
//! str0m requires every mutation of an `Rtc` to be followed by draining its output
//! to a timeout before the next one. The agent owns that cycle: [`Agent::rtc`] is
//! read only, and the [`RtcMut`] guard from [`Agent::rtc_mut`] drains when dropped,
//! queueing the events for [`Agent::next`].
//!
//! A dual-stack socket (`[::]`) reports an IPv4 peer as an IPv4-mapped IPv6
//! address and refuses a send to a real `V4` destination. Neither form leaks past
//! this crate: inbound sources are unmapped by [`canonical`], since ICE pairs
//! candidates by address family, and outbound destinations are mapped back.

#![warn(missing_docs)]

mod agent;
mod wire;

pub use agent::{Agent, Error, Event, RtcMut, canonical};
pub use wire::{INBOX, Inbound, Packet, Socket, Stats, Wire, reader};
