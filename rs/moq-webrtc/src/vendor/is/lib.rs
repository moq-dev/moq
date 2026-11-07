//! # is -- ICE in Sans-IO style
//!
//! Standalone [ICE][rfc8445] agent extracted from the [str0m][str0m] WebRTC library.
//!
//! ICE is the protocol used to establish peer-to-peer connectivity across
//! the internet. It coordinates STUN connectivity checks over every
//! possible network path between two peers and picks the best one,
//! handling NAT traversal along the way.
//!
//! This crate implements the ICE agent -- the state machine that drives
//! candidate pairing, connectivity checks, and nomination -- without
//! performing any I/O itself.
//!
//! # Sans-IO
//!
//! This is a [Sans-IO][sansio] implementation. The `IceAgent` never touches
//! a socket. Instead you drive it in a loop:
//!
//! 1. `poll_timeout()` -- when to wake up next.
//! 2. `handle_timeout(now)` -- advance timers.
//! 3. `poll_transmit()` -- STUN packets to send.
//! 4. `handle_packet(now, pkt)` -- feed incoming STUN.
//! 5. `poll_event()` -- state changes and nominations.
//!
//! This makes the agent easy to embed in any async runtime, event loop, or
//! test harness.
//!
//! # Quick start
//!
//! ```no_run
//! use is::{IceAgent, IceCreds, Candidate};
//!
//! // Create an agent with random credentials.
//! let mut agent = IceAgent::new(IceCreds::new());
//!
//! // Tell the agent about a local socket.
//! let addr = "192.168.1.100:5000".parse().unwrap();
//! agent.add_local_candidate(Candidate::host(addr, "udp").unwrap());
//!
//! // Exchange credentials and candidates with the remote peer via your
//! // signalling channel, then run the loop above until
//! // IceConnectionState::Connected.
//! ```
//!
//! # Address discovery is external
//!
//! The agent does not enumerate network interfaces, query STUN servers, or
//! allocate TURN relays. All of that is the caller's responsibility.
//! Discovered addresses are fed in via `add_local_candidate()` at any time
//! (trickle ICE).
//!
//! # Relationship to str0m
//!
//! This crate was extracted from [str0m][str0m] so the ICE agent can be
//! reused outside the full WebRTC stack. str0m depends on this crate with
//! default features disabled (it supplies its own HMAC provider through its
//! pluggable crypto backend).
//!
//! When used standalone the default `sha1` feature provides
//! `DefaultSha1HmacProvider` and a convenient `IceAgent::new()` constructor.
//! Disable it if you want to supply your own provider via
//! `IceAgent::with_hmac()`.
//!
//! # Design decisions
//!
//! The implementation makes several simplifying assumptions that suit
//! WebRTC usage:
//!
//! - **STUN/TURN are external** -- see above.
//! - **BUNDLE assumed** -- one agent, one data stream, one component (RTP).
//! - **Trickle ICE only** -- candidates can arrive at any time.
//! - **No frozen check state** -- single data stream means pairs go straight
//!   to waiting.
//! - **Nomination is not final** -- the agent can change its nominated pair
//!   as network conditions shift, without an ICE restart.
//! - **Ice-lite supported** -- the agent can act as a controlled ice-lite
//!   endpoint for server-side use.
//!
//! For the full rationale and list of assumptions see the
//! [design document][design].
//!
//! # Features
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `sha1`  | yes     | Provides `DefaultSha1HmacProvider` and `IceAgent::new()` |
//! | `pii`   | no      | Redacts IP addresses and other PII from log output |
//!
//! [rfc8445]: https://datatracker.ietf.org/doc/html/rfc8445
//! [str0m]: https://crates.io/crates/str0m
//! [sansio]: https://sans-io.readthedocs.io/
//! [design]: https://github.com/algesten/str0m/blob/main/docs/ice.md

// Avoid rewriting existing code: nested conditions are often more readable than long if-chains.
#![allow(clippy::collapsible_if)]
#![allow(clippy::new_without_default)]
#![allow(clippy::bool_to_int_with_if)]

#[macro_use]
extern crate tracing;

mod agent;
pub use agent::{IceAgent, IceAgentEvent, IceConnectionState, IceCreds, LocalPreference};

mod candidate;
pub use candidate::{Candidate, CandidateBuilder, CandidateKind};

mod pair;

mod preference;
pub use preference::default_local_preference;

mod error;
pub use error::IceError;
pub use error::StunError;

pub mod stun;

mod parse;

mod hmac_provider;
#[cfg(feature = "sha1")]
pub use hmac_provider::DefaultSha1HmacProvider;
pub use str0m_proto::{Protocol, Sha1HmacProvider};

#[cfg(test)]
pub(crate) mod test {
    use crate::agent::IceAgentStats;
    use crate::stun::{StunMessage, StunPacket};
    use crate::*;
    use str0m_proto::Protocol;
    use str0m_proto::Transmit;

    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::ops::{Deref, DerefMut};
    use std::time::{Duration, Instant};

    use tracing::Span;
    use tracing_subscriber::util::SubscriberInitExt;

    #[test]
    pub fn drop_host() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        // 9999 is just dropped by propagate
        let c1 = a1.add_host_candidate("1.1.1.1:9999");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_disconnected() && a2.state().is_disconnected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert_eq!(
            a1.stats(),
            IceAgentStats {
                bind_request_sent: 9,
                bind_success_recv: 0,
                bind_request_recv: 0,
                discovered_recv_count: 0,
                nomination_send_count: 0,
            }
        );

        assert_eq!(
            a2.stats(),
            IceAgentStats {
                bind_request_sent: 9,
                bind_success_recv: 0,
                bind_request_recv: 0,
                discovered_recv_count: 0,
                nomination_send_count: 0,
            }
        );
    }

    #[test]
    pub fn host_host_disconnect() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        a1.drop_sent_packets = true;

        loop {
            if !a1.state().is_connected() && !a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        println!("{:?}", a1.events);
        println!("{:?}", a2.events);

        fn assert_last_event(d: &Duration, e: &IceAgentEvent, a: &IceAgent) {
            assert_eq!(
                *e,
                IceAgentEvent::IceConnectionStateChange(IceConnectionState::Disconnected)
            );
            assert!(*d > a.ice_timeout());
        }

        let (d, e) = a1.events.last().unwrap();
        assert_last_event(d, e, &a1);

        let (d, e) = a2.events.last().unwrap();
        assert_last_event(d, e, &a2);

        assert_eq!(
            a1.stats(),
            IceAgentStats {
                bind_request_sent: 11,
                bind_success_recv: 2,
                bind_request_recv: 11,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );

        assert_eq!(
            a2.stats(),
            IceAgentStats {
                bind_request_sent: 11,
                bind_success_recv: 2,
                bind_request_recv: 2,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );
    }

    #[test]
    pub fn host_host() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert_eq!(
            a1.stats(),
            IceAgentStats {
                bind_request_sent: 2,
                bind_success_recv: 2,
                bind_request_recv: 2,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );

        assert_eq!(
            a2.stats(),
            IceAgentStats {
                bind_request_sent: 3,
                bind_success_recv: 2,
                bind_request_recv: 2,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );
    }

    // RFC 8445 §7.3.1.1: when both agents claim the controlling role, the
    // role conflict is resolved deterministically by the tiebreaker — the
    // higher-tiebreaker agent keeps the role, the other yields. Tiebreakers
    // here are the random ones picked by the constructor, so we cannot
    // predict which side wins; we only assert that the conflict resolves:
    // the connection still establishes and exactly one side ends up
    // controlled.
    #[test]
    pub fn role_conflict_resolves_to_one_controlled_side() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(true);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert!(
            a1.controlling() ^ a2.controlling(),
            "role conflict must leave exactly one side controlling, got a1={} a2={}",
            a1.controlling(),
            a2.controlling(),
        );
        assert!(
            a1.stats().nomination_send_count > 0 || a2.stats().nomination_send_count > 0,
            "role conflict must not block nomination",
        );
    }

    // str0m performs calculations on `now` internally
    // To ensure that these never panic, we run a happy-path of `host-host` that uses a very early `Instant`.
    #[test]
    pub fn happy_path_very_early_timestamp() {
        let early_now = find_earliest_now();

        let mut a1 = TestAgent::new(info_span!("L"));
        a1.time = early_now;
        let mut a2 = TestAgent::new(info_span!("R"));
        a2.time = early_now;

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
    }

    #[test]
    pub fn no_respond_to_stun_request_on_invalidated_candidate() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1.clone());

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        a1.agent.invalidate_candidate(&c1);

        let timeout = a2.poll_timeout().unwrap();
        a2.handle_timeout(timeout);
        let transmit = a2.poll_transmit().unwrap();

        assert!(a1.poll_transmit().is_none());

        a1.handle_packet(
            Instant::now(),
            StunPacket {
                proto: Protocol::Udp,
                source: sock("2.2.2.2:1000"),
                destination: sock("1.1.1.1:1000"),
                message: StunMessage::parse(&transmit.contents).unwrap(),
            },
        );

        assert!(a1.poll_transmit().is_none());
    }

    #[test]
    pub fn migrates_to_new_candidates_after_invalidation_without_timeout() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .set_default();

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = host("1.1.1.1:1000", "udp");
        let c1 = a1.add_local_candidate(c1).unwrap().clone();
        a2.add_remote_candidate(c1.clone());

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        let a1_time = a1.time;
        let a2_time = a2.time;
        let new_sock = sock("8.8.8.8:1000");

        let c3 = Candidate::host(new_sock, Protocol::Udp).unwrap();
        let c3 = a1.add_local_candidate(c3).unwrap().clone();
        a2.add_remote_candidate(c3);

        a1.agent.invalidate_candidate(&c1);
        a2.agent.invalidate_candidate(&c1);

        loop {
            let a1_nominated = a1.has_event(
                |e| matches!(e, IceAgentEvent::NominatedSend { source, .. } if source == &new_sock),
            );
            let a2_nominated = a2.has_event(
                |e| matches!(e, IceAgentEvent::NominatedSend { destination, .. } if destination == &new_sock)
            );

            if a1_nominated && a2_nominated {
                break;
            }

            progress(&mut a1, &mut a2);
        }

        assert!(a1.time.duration_since(a1_time) < a1.ice_timeout());
        assert!(a2.time.duration_since(a2_time) < a2.ice_timeout());
    }

    #[test]
    pub fn re_adding_invalidated_local_candidate() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1.clone());

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        a1.agent.invalidate_candidate(&c1);

        // Let time pass until it disconnects.
        loop {
            if a1.state().is_disconnected() && a2.state().is_disconnected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Add back the invalidated candidate
        a1.add_local_candidate(c1).unwrap();

        // progress() fails after 100 number of polls.
        a1.progress_count = 0;
        a2.progress_count = 0;
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
    }

    #[test]
    pub fn re_adding_invalidated_remote_candidate() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2.clone());

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        a1.agent.invalidate_candidate(&c2);

        // Let time pass until it disconnects.
        loop {
            if a1.state().is_disconnected() && a2.state().is_disconnected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Add back the invalidated candidate
        a1.add_remote_candidate(c2);

        // progress() fails after 100 number of polls.
        a1.progress_count = 0;
        a2.progress_count = 0;
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
    }

    #[test]
    pub fn ice_lite_no_connection() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        // a1 acts as "server"
        a1.agent.set_ice_lite(true);

        // 9999 is just dropped by propagate
        let c1 = a1.add_host_candidate("1.1.1.1:9999");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            // The bug we try to avoid is that _both sides_ must reach a disconnected state eventually.
            // The ice-lite (server side) should time out after roughly 8 seconds.
            if a1.state().is_disconnected() && a2.state().is_disconnected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert!(a1.time - a1.start_time > Duration::from_secs(8));

        assert_eq!(
            a1.stats(),
            IceAgentStats {
                bind_request_sent: 0,
                bind_success_recv: 0,
                bind_request_recv: 0,
                discovered_recv_count: 0,
                nomination_send_count: 0,
            }
        );

        assert_eq!(
            a2.stats(),
            IceAgentStats {
                bind_request_sent: 9,
                bind_success_recv: 0,
                bind_request_recv: 0,
                discovered_recv_count: 0,
                nomination_send_count: 0,
            }
        );
    }

    /// Regression test: ice-lite agents should not disconnect immediately on first timeout.
    #[test]
    pub fn ice_lite_no_immediate_disconnect() {
        let mut a1 = TestAgent::new(info_span!("L"));
        a1.set_ice_lite(true);

        a1.add_host_candidate("1.1.1.1:1000");
        a1.add_remote_candidate(host("2.2.2.2:2000", "udp"));

        a1.span.in_scope(|| a1.agent.handle_timeout(a1.time));

        assert!(!a1.state().is_disconnected());
    }

    #[test]
    pub fn prflx_host() {
        let mut a1 = TestAgent::new(info_span!("L")).with_port_restricted_nat("4.4.4.4");
        let mut a2 = TestAgent::new(info_span!("R"));

        // will be rewritten to 4.4.4.4
        let c1 = a1.add_host_candidate("3.3.3.3:1000");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert_eq!(
            a1.stats(),
            IceAgentStats {
                bind_request_sent: 2,
                bind_success_recv: 2,
                bind_request_recv: 1,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );

        assert_eq!(
            a2.stats(),
            IceAgentStats {
                bind_request_sent: 3,
                bind_success_recv: 1,
                bind_request_recv: 2,
                discovered_recv_count: 1,
                nomination_send_count: 1,
            }
        );
    }

    // pub fn init_log() {
    //     use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    //     let env_filter =
    //         EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"));

    //     tracing_subscriber::registry()
    //         .with(fmt::layer())
    //         .with(EnvFilter::from_default_env())
    //         .init();
    // }

    #[test]
    pub fn ice_lite_disconnect_reconnect() {
        // init_log();

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1.clone());

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2.clone());

        a1.set_controlling(true);
        a2.set_controlling(false);
        a2.set_ice_lite(true);

        // Connect.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Disconnect
        a1.drop_sent_packets = true;

        loop {
            if !a1.state().is_connected() && !a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Now reconnect after disconnecting.
        a1.drop_sent_packets = false;

        // Adding back the remote candidate will for new pairs to try.
        // This makes a1 start sending new STUN requests.
        // a2 in ice-lite mode will discover the pair from the STUN
        // request and come out of disconnected.
        a1.add_remote_candidate(c2);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
    }

    #[test]
    pub fn trickle_host_host() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        // no traffic possible
        let c1 = a1.add_host_candidate("3.3.3.3:9999");
        a2.add_remote_candidate(c1);

        let c2 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // this will be going nowhere (tested in drop-host.rs).
        for _ in 0..10 {
            progress(&mut a1, &mut a2);
        }

        // "trickle" a possible candidate
        a1.add_host_candidate("1.1.1.1:1000");

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
    }

    #[test]
    pub fn candidate_pair_of_same_kind_does_not_get_nominated() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R")).with_port_restricted_nat("4.4.4.4");

        let c1 = a1.add_relay_candidate("1.1.1.1:1000", "9.9.9.9:2000");
        a2.add_remote_candidate(c1);

        let c2 = a2.server_reflexive_candidate("8.8.8.8:3478", "3.3.3.3:1000");
        let c2 = a2.add_local_candidate(c2).unwrap().clone();
        a1.add_remote_candidate(c2);
        let c3 = a2.add_host_candidate("3.3.3.3:1000");
        a1.add_remote_candidate(c3);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // loop until we're connected.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        a2.add_remote_candidate(a1.add_relay_candidate("1.1.1.1:1001", "9.9.9.9:2000"));

        loop {
            if a2.has_event(|e| {
                matches!(e, IceAgentEvent::DiscoveredRecv { source, .. } if source == &sock("1.1.1.1:1001"))
            }) {
                break;
            }

            progress(&mut a1, &mut a2);
        }

        assert!(!a1.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, .. } if source == &sock("1.1.1.1:1001"))
        }));
        assert!(!a2.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { destination, .. }
                if destination == &sock("1.1.1.1:1001"))
        }));
    }

    #[test]
    pub fn no_disconnect_when_replacing_pflx_with_real_candidate() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        // We need a 2nd pair of candidates to make sure the agent doesn't go straight into `Completed`.

        // Both agents know their local candidates
        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        let c3 = a1.add_relay_candidate("2.2.2.2:1000", "9.9.9.9:2000");

        let c2 = a2.add_host_candidate("1.1.1.1:1001");
        let c4 = a2.add_relay_candidate("2.2.2.2:1001", "9.9.9.9:2000");

        // Agent 1 also learns about the remote candidates but agent 2
        // doesn't (imagine signalling layer being a bit slow)
        a1.add_remote_candidate(c2);
        a1.add_remote_candidate(c4);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // Wait until agent 2 is connected (based on a peer-reflexive candidate)
        loop {
            if a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }
        a2.events.clear();

        // Candidates arrive via signalling layer
        a2.add_remote_candidate(c1.clone());
        a2.add_remote_candidate(c3.clone());

        // Continue normal operation.
        for _ in 0..50 {
            progress(&mut a1, &mut a2);
        }

        // We expect to not disconnect as part of this, nor to go back to checking:
        // the replacing pair keeps the validated path of the peer-reflexive one.
        assert!(!a2.has_event(|e| matches!(
            e,
            IceAgentEvent::IceConnectionStateChange(
                IceConnectionState::Disconnected | IceConnectionState::Checking
            )
        )));
    }

    #[test]
    pub fn pair_replacement_in_attempt_window_stays_connected() {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        let c2 = a2.add_host_candidate("2.2.2.2:1000");

        // Only the controlling side initially knows the remote candidate. This makes
        // the controlled side discover a peer-reflexive remote from inbound STUN.
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // Exchange initial ICE credentials without consuming any binding requests.
        progress_without_network(&mut a1, &mut a2);
        progress_without_network(&mut a2, &mut a1);

        a1.events.clear();
        a2.events.clear();

        // The controlling side starts checking the signalled host candidate.
        progress_without_network(&mut a1, &mut a2);
        let initial_request = progress_one_message(&mut a1, &mut a2);
        assert!(initial_request.is_binding_request);
        assert!(!initial_request.use_candidate);

        let initial_reply = progress_one_message(&mut a2, &mut a1);
        assert!(initial_reply.is_successful_binding_response);
        assert!(a1.has_event(|e| matches!(e, IceAgentEvent::NominatedSend { .. })));

        // Now the controlling side sends the nominated USE-CANDIDATE request.
        progress_without_network(&mut a1, &mut a2);

        let nominate_request = progress_one_message(&mut a1, &mut a2);
        assert!(nominate_request.is_binding_request);
        assert!(nominate_request.use_candidate);

        // The controlled side answers that request and then sends a reverse binding
        // request to confirm the nomination. The reply is queued when the request
        // arrives, then the reverse binding request is queued on the next timeout.
        // The nominated pair is not valid until that request is answered (RFC 8445
        // §7.3.1.5), so the controlled side is not connected yet.
        progress_without_network(&mut a2, &mut a1);
        assert!(!a2.state().is_connected());

        let nominate_reply = a2.poll_next_stun_message();
        assert!(nominate_reply.is_successful_binding_response);
        let reverse_request = a2.poll_next_stun_message();
        assert!(reverse_request.is_binding_request);

        // Before the reverse binding response arrives, the signalling layer delivers the
        // controller's real remote candidate. This replaces the prflx pair during Attempt.
        a2.add_remote_candidate(c1);
        progress_without_network(&mut a2, &mut a1);

        assert!(!a2.state().is_connected());
        assert!(!a2.has_event(|e| matches!(
            e,
            IceAgentEvent::IceConnectionStateChange(IceConnectionState::Checking)
        )));

        // Finish the delayed round trip and ensure the controlled side stays connected.
        deliver_transmit(&mut a2, &mut a1, &nominate_reply.transmit);
        deliver_transmit(&mut a2, &mut a1, &reverse_request.transmit);

        let reverse_reply = progress_one_message(&mut a1, &mut a2);
        assert!(reverse_reply.is_successful_binding_response);

        for _ in 0..20 {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert!(a1.state().is_connected());
        assert!(a2.state().is_connected());
        assert!(!a2.has_event(|e| matches!(
            e,
            IceAgentEvent::IceConnectionStateChange(IceConnectionState::Checking)
        )));
    }

    #[test]
    pub fn identical_host_and_server_reflexive_candidates_dont_create_new_pairs_on_inbound_stun_request()
     {
        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = a1.add_host_candidate("1.1.1.1:1000");
        a2.add_remote_candidate(c1);
        let c2 = a2.server_reflexive_candidate("8.8.8.8:3478", "2.2.2.2:1000");
        let c2 = a2.add_local_candidate(c2).unwrap().clone();
        a1.add_remote_candidate(c2);
        let c3 = a2.add_host_candidate("2.2.2.2:1000");
        a1.add_remote_candidate(c3);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // loop until we're connected.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Each agent should only have a single candidate pair.
        assert_eq!(a1.num_candidate_pairs(), 1);
        assert_eq!(a2.num_candidate_pairs(), 1);
    }

    // In general, ICE prefers IPv6 over IPv4.
    // However, in case our only IPv6 connectivity is via a relay that we are talking to over IPv4,
    // we want to prefer the IPv4 code path.
    #[test]
    fn prefers_ipv4_ipv4_relay_candidate_over_ipv4_ipv6_controlling() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .set_default();

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        a1.set_controlling(true);
        a2.set_controlling(false);

        prefer_ipv4_candidate_over_ipv6_candidate(&mut a1, &mut a2);
    }

    // In general, ICE prefers IPv6 over IPv4.
    // However, in case our only IPv6 connectivity is via a relay that we are talking to over IPv4,
    // we want to prefer the IPv4 code path.
    #[test]
    fn prefers_ipv4_ipv4_relay_candidate_over_ipv4_ipv6_controlled() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .set_default();

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        a1.set_controlling(false);
        a2.set_controlling(true);

        prefer_ipv4_candidate_over_ipv6_candidate(&mut a1, &mut a2);
    }

    fn prefer_ipv4_candidate_over_ipv6_candidate(a1: &mut TestAgent, a2: &mut TestAgent) {
        // Agent 1 only has IPv4 connectivity to a relay but allocates both IPv4 and IPv6 addresses.
        // Agent 2 has no relay but has full IPv4 and IPv6 connectivity.
        let relay_ipv4_ipv4 = a1.add_relay_candidate("7.7.7.7:5000", "1.1.1.1:5000");
        let relay_ipv6_ipv4 = a1.add_relay_candidate("[::7]:5000", "1.1.1.1:5000");
        a2.add_remote_candidate(relay_ipv4_ipv4);
        a2.add_remote_candidate(relay_ipv6_ipv4);

        let host_ipv4 = a2.add_host_candidate("5.5.5.5:3000");
        let host_ipv6 = a2.add_host_candidate("[::2]:3000");
        a1.add_remote_candidate(host_ipv4);
        a1.add_remote_candidate(host_ipv6);

        // loop until we're connected.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(a1, a2);
        }

        assert!(a1.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("7.7.7.7:5000") && destination == &sock("5.5.5.5:3000"))
        }));
        assert!(a2.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("5.5.5.5:3000") && destination == &sock("7.7.7.7:5000"))
        }));
    }

    #[test]
    fn changed_timing_config_takes_effect_immediately() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        const IDLE_RTO: Duration = Duration::from_secs(60);
        const NORMAL_RTO: Duration = Duration::from_secs(3);

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = host("1.1.1.1:1000", "udp");
        a1.add_local_candidate(c1.clone());
        a2.add_remote_candidate(c1);

        let c2 = host("2.2.2.2:1000", "udp");
        a2.add_local_candidate(c2.clone());
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // loop until we're connected.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Move to "idle" mode
        a1.set_initial_stun_rto(IDLE_RTO);
        a1.set_max_stun_rto(IDLE_RTO);
        a2.set_initial_stun_rto(IDLE_RTO);
        a2.set_max_stun_rto(IDLE_RTO);

        // Spin for a bit
        for _ in 0..10 {
            progress(&mut a1, &mut a2);
        }

        // This is a bit of a hack because we use "insider" knowledge here
        // that the next timeout is in fact `IDLE_RTO` away.
        let now = a1.poll_timeout().unwrap() - IDLE_RTO;

        a1.set_initial_stun_rto(NORMAL_RTO);
        a1.set_max_stun_rto(NORMAL_RTO);

        let timeout_after = a1.poll_timeout().unwrap();

        // After applying the new timeout, it should only be `NORMAL_RTO` away.
        assert_eq!(timeout_after, now + NORMAL_RTO);
    }

    #[test]
    fn new_candidates_after_disconnected_should_transition_to_checking() {
        let _guard = tracing_subscriber::fmt()
            .with_env_filter("trace")
            .with_test_writer()
            .set_default();

        let mut a1 = TestAgent::new(info_span!("L"));
        let mut a2 = TestAgent::new(info_span!("R"));

        let c1 = host("1.1.1.1:1000", "udp");
        a1.add_local_candidate(c1.clone());
        a2.add_remote_candidate(c1);

        let c2 = host("2.2.2.2:1000", "udp");
        a2.add_local_candidate(c2.clone());
        a1.add_remote_candidate(c2);

        a1.set_controlling(true);
        a2.set_controlling(false);

        // loop until we're connected.
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // signal network outage
        a1.drop_sent_packets = true;
        a2.drop_sent_packets = true;

        loop {
            if a1.state().is_disconnected() && a2.state().is_disconnected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // Reconnect with new candidates
        a1.drop_sent_packets = false;
        a2.drop_sent_packets = false;

        let c1 = host("5.5.5.5:1000", "udp");
        a1.add_local_candidate(c1.clone());
        a2.add_remote_candidate(c1);

        let c2 = host("6.6.6.6:1000", "udp");
        a2.add_local_candidate(c2.clone());
        a1.add_remote_candidate(c2);

        // Clear all existing events
        a1.events.clear();
        a2.events.clear();

        // Reconnect
        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        assert!(a1.has_event(|e| matches!(
            e,
            IceAgentEvent::IceConnectionStateChange(IceConnectionState::Checking)
        )));
        assert!(a2.has_event(|e| matches!(
            e,
            IceAgentEvent::IceConnectionStateChange(IceConnectionState::Checking)
        )));
    }

    #[test]
    pub fn symmetric_nat_both_sides() {
        let _guard = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("trace")
            .set_default();

        // Both agents are behind symmetric NAT
        let mut a1 = TestAgent::new(info_span!("L")).with_symmetric_nat("5.5.5.5");
        let mut a2 = TestAgent::new(info_span!("R")).with_symmetric_nat("6.6.6.6");

        // Add host candidates. Those will fail behind NAT.
        a2.add_remote_candidate(a1.add_host_candidate("1.1.1.1:1000"));
        a1.add_remote_candidate(a2.add_host_candidate("2.2.2.2:1000"));

        // Add server-reflexive candidates, those will also fail because the NAT is symmetric
        a2.add_remote_candidate(a1.server_reflexive_candidate("8.8.8.8:3478", "1.1.1.1:1000"));
        a1.add_remote_candidate(a2.server_reflexive_candidate("8.8.8.8:3478", "2.2.2.2:1000"));

        // Add relay candidates, those will work
        a2.add_remote_candidate(a1.add_relay_candidate("3.3.3.3:1000", "1.1.1.1:1000"));
        a1.add_remote_candidate(a2.add_relay_candidate("4.4.4.4:1000", "2.2.2.2:1000"));

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // A1 sends from its host candidate with a random port getting assigned by the symmetric NAT.
        assert!(a1.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("1.1.1.1:1000") && destination == &sock("4.4.4.4:1000"))
        }));
        // A2 sends from its relay candidate, towards the public IP of A1.
        assert!(a2.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("4.4.4.4:1000") && destination.ip() == ip("5.5.5.5"))
        }));
    }

    #[test]
    pub fn symmetric_nat_one_side() {
        let _guard = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("trace")
            .set_default();

        // Only one agent is behind symmetric NAT.
        let mut a1 = TestAgent::new(info_span!("L")).with_symmetric_nat("5.5.5.5");
        let mut a2 = TestAgent::new(info_span!("R")).with_port_restricted_nat("6.6.6.6");

        // Add host candidates. Those will fail behind NAT.
        a2.add_remote_candidate(a1.add_host_candidate("1.1.1.1:1000"));
        a1.add_remote_candidate(a2.add_host_candidate("2.2.2.2:1000"));

        // Add server-reflexive candidates, those will also fail due to A1's symmetric NAT.
        a2.add_remote_candidate(a1.server_reflexive_candidate("8.8.8.8:3478", "1.1.1.1:1000"));
        a1.add_remote_candidate(a2.server_reflexive_candidate("8.8.8.8:3478", "2.2.2.2:1000"));

        // Add relay candidates, those will work.
        a2.add_remote_candidate(a1.add_relay_candidate("3.3.3.3:1000", "1.1.1.1:1000"));
        a1.add_remote_candidate(a2.add_relay_candidate("4.4.4.4:1000", "2.2.2.2:1000"));

        a1.set_controlling(true);
        a2.set_controlling(false);

        loop {
            if a1.state().is_connected() && a2.state().is_connected() {
                break;
            }
            progress(&mut a1, &mut a2);
        }

        // A1 sends from its host candidate with a random port getting assigned by the symmetric NAT.
        assert!(a1.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("1.1.1.1:1000") && destination == &sock("4.4.4.4:1000"))
        }));
        // A2 sends from its relay candidate, towards the public IP of A1.
        assert!(a2.has_event(|e| {
            matches!(e, IceAgentEvent::NominatedSend { source, destination, .. }
                         if source == &sock("4.4.4.4:1000") && destination.ip() == ip("5.5.5.5"))
        }));
    }

    #[test]
    fn server_reflexive_candidate_from_different_servers_have_different_ports_on_sym_nat() {
        let mut agent = TestAgent::new(info_span!("A")).with_symmetric_nat("4.4.4.4");

        let c1 = agent.server_reflexive_candidate("8.8.8.8:3478", "1.1.1.1:1000");
        let c2 = agent.server_reflexive_candidate("9.9.9.9:3478", "1.1.1.1:1000");

        assert_ne!(c1, c2);
    }

    #[test]
    fn server_reflexive_candidate_from_different_servers_are_equal_on_restricted_nat() {
        let mut agent = TestAgent::new(info_span!("A")).with_port_restricted_nat("4.4.4.4");

        let c1 = agent.server_reflexive_candidate("8.8.8.8:3478", "1.1.1.1:1000");
        let c2 = agent.server_reflexive_candidate("9.9.9.9:3478", "1.1.1.1:1000");

        assert_eq!(c1, c2);
    }

    pub struct TestStunMessage {
        transmit: Transmit,
        is_binding_request: bool,
        is_successful_binding_response: bool,
        use_candidate: bool,
    }

    pub struct TestAgent {
        pub start_time: Instant,
        pub agent: IceAgent,
        pub span: Span,
        pub events: Vec<(Duration, IceAgentEvent)>,
        pub progress_count: u64,
        pub time: Instant,
        pub drop_sent_packets: bool,
        pub nat: Option<Nat>,
    }

    impl TestAgent {
        pub fn new(span: Span) -> Self {
            let now = Instant::now();
            TestAgent {
                start_time: now,
                agent: IceAgent::new(IceCreds::new()),
                span,
                events: vec![],
                progress_count: 0,
                time: now,
                drop_sent_packets: false,
                nat: None,
            }
        }

        pub fn with_symmetric_nat(mut self, external_ip: &str) -> Self {
            self.nat = Some(Nat::new_symmetric(external_ip));
            self
        }

        pub fn with_port_restricted_nat(mut self, external_ip: &str) -> Self {
            self.nat = Some(Nat::new_port_restricted_cone(external_ip));
            self
        }

        fn add_host_candidate(&mut self, addr: &str) -> Candidate {
            self.span.in_scope(|| {
                self.agent
                    .add_local_candidate(host(addr, "udp"))
                    .unwrap()
                    .clone()
            })
        }

        fn add_relay_candidate(&mut self, addr: &str, local: &str) -> Candidate {
            let local = local.parse().unwrap();
            let addr = addr.parse().unwrap();

            self.span.in_scope(|| {
                self.agent
                    .add_local_candidate(Candidate::relayed(addr, local, "udp").unwrap())
                    .unwrap()
                    .clone()
            })
        }

        fn add_remote_candidate(&mut self, c: Candidate) {
            // Round-trip via SDP to simulate signalling protocol.
            let c = Candidate::from_sdp_string(&c.to_sdp_string()).unwrap();

            self.span.in_scope(|| self.agent.add_remote_candidate(c))
        }

        /// Simulate STUN to the given server and returned the server-reflexive candidate.
        ///
        /// Unlike [`TestAgent::add_host_candidate`] and [`TestAgent::add_relay_candidate`],
        /// this candidate is **not** added to the agent.
        /// Server-reflexive candidates are typically not added locally because one cannot
        /// directly send from them.
        /// The agent can only send from the base which is a host candidate.
        fn server_reflexive_candidate(&mut self, stun_server: &str, base: &str) -> Candidate {
            use NatType::*;
            let base = base.parse().unwrap();
            let stun_server = stun_server.parse().unwrap();

            match self.nat.as_mut() {
                None => Candidate::server_reflexive(base, base, "udp"),
                Some(Nat {
                    nat_type: PortRestrictedCone { .. },
                    external_ip,
                }) => Candidate::server_reflexive(
                    SocketAddr::new(*external_ip, base.port()),
                    base,
                    "udp",
                ),
                Some(Nat {
                    nat_type: Symmetric { mappings },
                    external_ip,
                }) => {
                    let outside = symmetric_nat_lookup(base, stun_server, mappings);

                    Candidate::server_reflexive(SocketAddr::new(*external_ip, outside), base, "udp")
                }
            }
            .unwrap()
        }

        fn has_event(&self, predicate: impl Fn(&IceAgentEvent) -> bool) -> bool {
            self.events.iter().any(|(_, e)| predicate(e))
        }

        fn advance_time(&mut self) {
            self.time = self
                .span
                .in_scope(|| self.agent.poll_timeout())
                .unwrap_or(self.time);
        }

        fn poll_next_stun_message(&mut self) -> TestStunMessage {
            let transmit = self.poll_transmit().expect("agent should emit a transmit");
            let message =
                StunMessage::parse(&transmit.contents).expect("IceAgent to only emit StunMessages");

            TestStunMessage {
                is_binding_request: message.is_binding_request(),
                is_successful_binding_response: message.is_successful_binding_response(),
                use_candidate: message.use_candidate(),
                transmit,
            }
        }
    }

    pub fn progress(a1: &mut TestAgent, a2: &mut TestAgent) {
        let (f, t) = if a1.progress_count % 2 == a2.progress_count % 2 {
            (a2, a1)
        } else {
            (a1, a2)
        };

        t.progress_count += 1;
        if t.progress_count > 100 {
            panic!("Test looped more than 100 times");
        }

        if let Some(trans) = f.span.in_scope(|| f.agent.poll_transmit()) {
            let message =
                StunMessage::parse(&trans.contents).expect("IceAgent to only emit StunMessages");

            // rewrite receive with test transforms, and potentially drop the packet.
            if let Some((source, destination)) = transform(trans.source, trans.destination, f, t) {
                if f.drop_sent_packets {
                    // drop packet
                    t.span.in_scope(|| t.agent.handle_timeout(t.time));
                } else {
                    if source != trans.source {
                        f.span.in_scope(|| {
                            tracing::trace!("Outbound NAT: {} => {}", trans.source, source)
                        });
                    }

                    if destination != trans.destination {
                        t.span.in_scope(|| {
                            tracing::trace!("Inbound NAT: {} => {}", trans.destination, destination)
                        });
                    }

                    let packet = StunPacket {
                        proto: trans.proto,
                        source,
                        destination,
                        message,
                    };
                    t.span.in_scope(|| t.agent.handle_packet(t.time, packet));
                }
            } else {
                tracing::debug!(from = %trans.source, to = %trans.destination, "NAT: Dropping packet");

                // drop packet
                t.span.in_scope(|| t.agent.handle_timeout(t.time));
            }
        } else {
            t.span.in_scope(|| t.agent.handle_timeout(t.time));
        }

        let time = t.time;

        let tim_f = f.span.in_scope(|| f.agent.poll_timeout()).unwrap_or(f.time);
        f.time = tim_f;

        let tim_t = t.span.in_scope(|| t.agent.poll_timeout()).unwrap_or(t.time);
        t.time = tim_t;

        while let Some(v) = t.span.in_scope(|| t.agent.poll_event()) {
            println!("Polled event: {v:?}");
            use IceAgentEvent::*;
            f.span.in_scope(|| {
                if let IceRestart(v) = &v {
                    f.agent.set_remote_credentials(v.clone())
                }
            });
            t.events.push((time - t.start_time, v));
        }
    }

    // These helpers intentionally bypass `progress()` so the regression test above
    // can hold specific STUN packets and inject the signalled host candidate in the
    // exact Attempt window being tested.
    fn progress_without_network(from: &mut TestAgent, to: &mut TestAgent) {
        let now = from.time;
        from.handle_timeout(now);
        drain_manual_test_events(to, from);
        from.advance_time();
        to.advance_time();
    }

    fn drain_manual_test_events(from: &mut TestAgent, to: &mut TestAgent) {
        let time = to.time;
        while let Some(v) = to.span.in_scope(|| to.agent.poll_event()) {
            println!("Polled event: {v:?}");
            use IceAgentEvent::*;
            from.span.in_scope(|| {
                if let IceRestart(v) = &v {
                    from.agent.set_remote_credentials(v.clone())
                }
            });
            to.events.push((time - to.start_time, v));
        }
    }

    fn progress_one_message(from: &mut TestAgent, to: &mut TestAgent) -> TestStunMessage {
        let msg = from.poll_next_stun_message();

        deliver_transmit(from, to, &msg.transmit);

        msg
    }

    fn deliver_transmit(from: &mut TestAgent, to: &mut TestAgent, trans: &Transmit) {
        let message =
            StunMessage::parse(&trans.contents).expect("IceAgent to only emit StunMessages");
        let packet = StunPacket {
            proto: trans.proto,
            source: trans.source,
            destination: trans.destination,
            message,
        };
        to.span.in_scope(|| to.agent.handle_packet(to.time, packet));
        drain_manual_test_events(from, to);
        from.advance_time();
        to.advance_time();
    }

    impl Deref for TestAgent {
        type Target = IceAgent;

        fn deref(&self) -> &Self::Target {
            &self.agent
        }
    }

    impl DerefMut for TestAgent {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.agent
        }
    }

    /// Performs a binary search for the earliest possible `Instant`.
    fn find_earliest_now() -> Instant {
        const ONE_YEAR: Duration = Duration::from_secs(60 * 60 * 24 * 365);

        let mut now = Instant::now();
        let mut step = ONE_YEAR;

        while step > Duration::from_secs(1) {
            match now.checked_sub(step) {
                Some(earlier) => {
                    now = earlier;
                    step *= 2; // Increase step-count to accelerate finding the earliest possible `Instant`.
                }
                None => {
                    step /= 2; // Decrease step-count to narrow down on the earliest possible `Instant`.
                }
            }
        }

        now
    }

    pub fn sock(s: impl Into<String>) -> SocketAddr {
        let s: String = s.into();
        s.parse().unwrap()
    }

    pub fn ip(s: impl Into<String>) -> IpAddr {
        let s: String = s.into();
        s.parse().unwrap()
    }

    pub fn host(s: impl Into<String>, proto: impl TryInto<Protocol>) -> Candidate {
        Candidate::host(sock(s), proto).unwrap()
    }

    #[derive(Debug, Clone)]
    enum NatType {
        PortRestrictedCone {
            mappings: HashMap<(SocketAddr, SocketAddr), u16>,
        },
        Symmetric {
            mappings: HashMap<(SocketAddr, SocketAddr), u16>,
        },
    }

    #[derive(Debug, Clone)]
    pub struct Nat {
        external_ip: IpAddr,
        nat_type: NatType,
    }

    impl Nat {
        fn new_port_restricted_cone(external_ip: &str) -> Self {
            Self {
                external_ip: external_ip.parse().expect("Invalid IP address"),
                nat_type: NatType::PortRestrictedCone {
                    mappings: Default::default(),
                },
            }
        }

        fn new_symmetric(external_ip: &str) -> Self {
            Self {
                external_ip: external_ip.parse().expect("Invalid IP address"),
                nat_type: NatType::Symmetric {
                    mappings: Default::default(),
                },
            }
        }

        fn transform_outbound(
            &mut self,
            from: SocketAddr,
            to: SocketAddr,
        ) -> (SocketAddr, SocketAddr) {
            let external_port = match &mut self.nat_type {
                NatType::Symmetric { mappings } => {
                    // For symmetric NATs, the key is the 4-tuple of the connection.

                    symmetric_nat_lookup(from, to, mappings)
                }
                NatType::PortRestrictedCone { mappings } => {
                    // For a port-restricted-cone NAT, the key is also the 4-tuple of the connection.
                    // Contrary to a symmetric NAT, the source port is preserved.
                    *mappings.entry((from, to)).or_insert(from.port())
                }
            };

            (SocketAddr::new(self.external_ip, external_port), to)
        }

        fn transform_inbound(
            &self,
            from: SocketAddr,
            to: SocketAddr,
        ) -> Option<(SocketAddr, SocketAddr)> {
            let internal_addr = match &self.nat_type {
                // For symmetric NAT, need exact source match of from == dst and only on the assigned port.
                NatType::Symmetric { mappings } => {
                    let ((src, _), _) = mappings
                        .iter()
                        .find(|((_, dst), outside)| from == *dst && **outside == to.port())?;

                    *src
                }
                // For restricted-cone NAT, traffic on the outside assigned port is routed back
                // if we have previously contacted this IP + port combination.
                NatType::PortRestrictedCone { mappings } => {
                    let ((src, _), _) = mappings
                        .iter()
                        .find(|((_, dst), outside)| from == *dst && **outside == to.port())?;

                    *src
                }
            };

            Some((from, internal_addr))
        }
    }

    fn symmetric_nat_lookup(
        src: SocketAddr,
        dst: SocketAddr,
        mappings: &mut HashMap<(SocketAddr, SocketAddr), u16>,
    ) -> u16 {
        let outside_port = loop {
            let port = str0m_proto::NonCryptographicRng::u16();

            if mappings.values().any(|p| *p == port) {
                continue;
            }

            break port;
        };

        *mappings.entry((src, dst)).or_insert(outside_port)
    }

    /// Transform function with flexible NAT support
    fn transform(
        from: SocketAddr,
        to: SocketAddr,
        from_agent: &mut TestAgent,
        to_agent: &mut TestAgent,
    ) -> Option<(SocketAddr, SocketAddr)> {
        if from.port() == 9999 || to.port() == 9999 {
            return None;
        }

        let outgoing_is_from_relay = from_agent
            .local_candidates()
            .any(|c| c.addr() == from && c.kind() == CandidateKind::Relayed);

        // If NAT is present on sending agent, apply it.
        let (from, to) = if let Some(nat) = &mut from_agent.nat {
            // If our traffic is "from" a relay candidate, the NAT does not apply.
            if outgoing_is_from_relay {
                (from, to)
            } else {
                nat.transform_outbound(from, to)
            }
        } else {
            (from, to)
        };

        let incoming_is_from_relay = to_agent
            .local_candidates()
            .any(|c| c.addr() == to && c.kind() == CandidateKind::Relayed);

        // If NAT is present on receiving agent, apply it.
        if let Some(nat) = &mut to_agent.nat {
            // If our traffic is "to" a relay candidate, the NAT does not apply.
            if incoming_is_from_relay {
                Some((from, to))
            } else {
                if nat.external_ip != to.ip() {
                    return None;
                }

                nat.transform_inbound(from, to)
            }
        } else {
            Some((from, to))
        }
    }
}
