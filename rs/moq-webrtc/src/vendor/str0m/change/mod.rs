//! Ways to change the [`Rtc`][crate::Rtc] session. SDP or Direct.
//!
//! str0m has two main APIs for changing the WebRTC session.
//!
//! 1. SDP API. The common way to talk to browsers using SDP OFFER/ANSWER negotiations.
//!    [`Rtc::sdp_api()`][crate::Rtc::sdp_api]
//! 2. Direct API. Makes changes directly to the session without any negotiation.
//!    [`Rtc::direct_api()`][crate::Rtc::direct_api]
//!
//! ## Direct API
//!
//! The direct API is a lower level API which typically can't be mixed with the SDP API. If you make
//! changes directly to the session, the remote side would not be aware of them unless you construct
//! some "other way" keeping the two peers in sync.
mod sdp;
pub(crate) use sdp::AddMedia;
pub use sdp::{SdpAnswer, SdpApi, SdpOffer, SdpPendingOffer};

mod direct;
pub use direct::DirectApi;
