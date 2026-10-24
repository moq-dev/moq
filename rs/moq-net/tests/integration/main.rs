//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `loom.rs`, `wasm.rs`, and `send.rs` stay apart because they build for
//! another cfg, target, or toolchain. `group_charge.rs` stays apart because its
//! counting global allocator would apply to every test in the binary.

mod announce_covering_route;
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
mod empty_replacement;
mod extensions;
mod fetch_cancel;
mod fetch_fanout;
mod fetch_from_feed;
mod fetch_only;
mod finished_broadcast_mock;
mod finite_relay;
mod goaway;
mod group_order;
mod head_fetch;
mod history_groups;
mod idle_cancel_refetch;
mod idle_session;
mod ietf_conformance;
mod implementation;
mod late_lower_group;
mod lazy_discovery;
mod mesh_withdraw;
mod model;
mod multi_cdn;
mod partial_group;
mod quiet_catalog;
mod received_rewind;
mod refresh;
mod rejoin;
mod replacement;
mod request_caps;
mod restart;
mod route_change;
mod routing_contracts;
mod session_close;
mod session_panic;
mod settled_end_survives_abort;
mod source_errors;
mod stream_credit;
mod subscribe_priority;
mod subscribe_ranges;
mod subscribe_tracks;
mod subscriber_max_age;
mod subscription_end_integrity;
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
mod wide_sequence;
mod zero_copy;
