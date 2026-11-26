//! Integration tests without feature-enabling dependencies, linked into one binary.
//!
//! Tests needing JSON consumers or the `fuzz` module live in `moq-net-tests`.
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `send.rs` stays apart because it builds on another toolchain.
//! `group_charge.rs` stays apart because its counting global allocator would
//! apply to every test in the binary.

mod announce_covering_route;
mod announce_to_serve;
mod answer_behind_cache;
mod auth;
mod bridged_rank;
mod broadcast_close;
mod catalog_acquisition;
mod catalog_churn;
mod catalog_handover;
mod claim_continues;
mod claim_reach;
mod compress;
mod control_stats;
mod datagram;
mod dial_split_horizon;
mod drained_claim;
mod drop_record;
mod egress_stats;
mod empty_group;
mod errored_end;
mod extensions;
mod failover_chain;
mod fastest_reader;
mod fetch_cancel;
mod fetch_fanout;
mod fetch_from_feed;
mod fetch_only;
mod fetch_pipeline;
mod finished_broadcast_mock;
mod finished_copy_history;
mod finite_relay;
mod flights;
mod goaway;
mod group_end;
mod group_order;
mod half_read_frame;
mod history_groups;
mod history_join;
mod idle_cancel_refetch;
mod idle_session;
mod ietf_keys;
mod implementation;
mod instance;
mod late_lower_group;
mod late_tail_reader;
mod lazy_discovery;
mod lite06_fill_walk;
mod mesh_sweep;
mod mesh_withdraw;
mod min_age_join;
mod model;
mod mount;
mod multi_cdn;
mod nested_claim;
mod partial_group;
mod path_routes;
mod quiet_catalog;
mod range_anchors;
mod range_fill;
mod range_read;
mod range_table;
mod range_union_fit;
mod received_rewind;
mod rejoin;
mod relay_upstream_close;
mod remove_fetch;
mod replacement;
mod request_caps;
mod route_acceptance;
mod route_audit;
mod route_change;
mod route_cluster;
mod route_failover;
mod route_health;
mod route_race;
mod route_stats;
mod routeless_domain;
mod segmented_frame;
mod server_cost;
mod session_close;
mod session_panic;
mod settled_end_survives_abort;
mod setup_negotiation;
mod shared_copy_start;
mod slow_joiner;
mod source_errors;
mod stream_credit;
mod subscribe_priority;
mod subscribe_ranges;
mod subscribe_tracks;
mod subscribe_with_track;
mod subscriber_max_age;
mod subscription_end_integrity;
#[cfg(feature = "async")]
mod support;
mod switch_frame_cap;
mod synced;
mod time_keyed_start;
mod time_start_fill;
mod track_abort_over_session;
mod track_info_cancel;
mod track_stream_demand;
mod track_tail;
mod tracks;
mod tracks_ietf;
mod unannounce_release;
mod untimed;
mod viewer_target;
mod waiting_time_start;
mod warm_backfill;
mod wide_key;
mod widened_max_age;
mod zero_copy;

/// A subscription under one staleness budget, as a subscription-wide max age once applied.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) trait WithMax {
	/// A range from the oldest or the latest group takes every group still within `max`, or
	/// every group for `None`; another live join keeps its lag and takes
	/// `max`; a time start stays history.
	fn with_max(self, max: impl Into<Option<std::time::Duration>>) -> Self;
}

impl WithMax for moq_net::track::Subscription {
	fn with_max(mut self, max: impl Into<Option<std::time::Duration>>) -> Self {
		use moq_net::track::Start;
		let max = max.into();
		for range in &mut self.ranges {
			range.start = match range.start {
				// No lag limit takes every group still within `max`.
				Start::Oldest | Start::LATEST => match max {
					Some(max) => Start::Live {
						min: None,
						max: Some(max),
					},
					None => Start::Oldest,
				},
				Start::Live { min, .. } => Start::Live { min, max },
				Start::Time(time) => Start::Time(time),
			};
		}
		self
	}
}
