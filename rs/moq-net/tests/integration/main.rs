//! Integration tests without feature-enabling dependencies, linked into one binary.
//!
//! Tests needing JSON consumers or the hidden `fuzz` module live in `moq-net-tests`.
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `send.rs` stays apart because it builds on another toolchain.
//! `group_charge.rs` stays apart because its counting global allocator would
//! apply to every test in the binary.

mod announce_covering_route;
mod announce_takeover;
mod announce_to_serve;
mod auth;
mod broadcast_close;
mod catalog_acquisition;
mod catalog_churn;
mod catalog_handover;
mod compress;
mod control_stats;
mod datagram;
mod dial_split_horizon;
mod drained_claim;
mod empty_group;
mod errored_end;
mod extensions;
mod fetch_cancel;
mod fetch_fanout;
mod fetch_from_feed;
mod fetch_only;
mod fetch_pipeline;
mod finished_broadcast_mock;
mod finite_relay;
mod goaway;
mod group_order;
mod history_groups;
mod idle_cancel_refetch;
mod idle_session;
mod implementation;
mod late_lower_group;
mod lazy_discovery;
mod mesh_withdraw;
mod model;
mod multi_cdn;
mod partial_group;
mod quiet_catalog;
mod range_anchors;
mod received_rewind;
mod rejoin;
mod relay_upstream_close;
mod replacement;
mod request_caps;
mod route_change;
mod routing_contracts;
mod segmented_frame;
mod session_close;
mod session_idle;
mod session_panic;
mod settled_end_survives_abort;
mod setup_negotiation;
mod slow_joiner;
mod source_errors;
mod stream_credit;
mod subscribe_priority;
mod subscribe_ranges;
mod subscribe_tracks;
mod subscriber_max_age;
mod subscription_end_integrity;
#[cfg(feature = "async")]
mod support;
mod synced;
mod track_abort_over_session;
mod track_info_cancel;
mod track_stream_demand;
mod track_tail;
mod tracks;
mod tracks_ietf;
mod unannounce_release;
mod untimed;
mod warm_backfill;
mod wide_hop;
mod wide_sequence;
mod widened_max_age;
mod zero_copy;
