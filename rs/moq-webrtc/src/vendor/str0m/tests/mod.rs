//! str0m's integration tests, run against the vendored module.

#[path = "abs-capture-time.rs"]
mod abs_capture_time;
#[path = "api-errors.rs"]
mod api_errors;
mod bidirectional;
#[path = "change-ssrc-reset-receive.rs"]
mod change_ssrc_reset_receive;
mod common;
#[path = "config-validation.rs"]
mod config_validation;
mod contiguous;
#[path = "crypto-compliance.rs"]
mod crypto_compliance;
#[path = "data-channel.rs"]
mod data_channel;
#[path = "data-channel-direct.rs"]
mod data_channel_direct;
#[path = "dtls-close.rs"]
mod dtls_close;
#[path = "dtls-error-readiness.rs"]
mod dtls_error_readiness;
#[path = "dtls-fingerprint.rs"]
mod dtls_fingerprint;
#[path = "dtls-retransmit.rs"]
mod dtls_retransmit;
#[path = "dtls-security.rs"]
mod dtls_security;
#[path = "early-rtx-padding.rs"]
mod early_rtx_padding;
#[path = "flappy-ice-state.rs"]
mod flappy_ice_state;
#[path = "handshake-direct.rs"]
mod handshake_direct;
#[path = "ice-candidates.rs"]
mod ice_candidates;
#[path = "ice-lite-connectivity.rs"]
mod ice_lite_connectivity;
#[path = "ice-remote-binding-keepalive.rs"]
mod ice_remote_binding_keepalive;
#[path = "ice-restart.rs"]
mod ice_restart;
#[path = "idle-rtc.rs"]
mod idle_rtc;
#[path = "keyframe-requests.rs"]
mod keyframe_requests;
mod keyframes;
mod loss;
#[path = "malformed-packet-injection.rs"]
mod malformed_packet_injection;
#[path = "mediatime-backwards.rs"]
mod mediatime_backwards;
#[path = "mtu-compliance.rs"]
mod mtu_compliance;
mod nack;
#[path = "network-conditions.rs"]
mod network_conditions;
#[path = "no-local-media-added.rs"]
mod no_local_media_added;
#[path = "no-pre-ice-feedback.rs"]
mod no_pre_ice_feedback;
#[path = "partial-init.rs"]
mod partial_init;
#[path = "pli-feedback.rs"]
mod pli_feedback;
#[path = "poll-scaling.rs"]
mod poll_scaling;
mod remb;
#[path = "remove-media-while-pacer-queued.rs"]
mod remove_media_while_pacer_queued;
mod repeated;
#[path = "rtcp-request-timeout.rs"]
mod rtcp_request_timeout;
#[path = "rtp-direct-csrc.rs"]
mod rtp_direct_csrc;
#[path = "rtp-direct-mid.rs"]
mod rtp_direct_mid;
#[path = "rtp-direct-mid-rid.rs"]
mod rtp_direct_mid_rid;
#[path = "rtp-direct-reset-ssrc.rs"]
mod rtp_direct_reset_ssrc;
#[path = "rtp-direct-ssrc.rs"]
mod rtp_direct_ssrc;
#[path = "rtp-direct-with-roc.rs"]
mod rtp_direct_with_roc;
#[path = "rtp-sequence.rs"]
mod rtp_sequence;
mod rtp_to_frame;
#[path = "rtx-cache-0.rs"]
mod rtx_cache_0;
#[path = "sdp-m-recycle.rs"]
mod sdp_m_recycle;
#[path = "sdp-negotiation.rs"]
mod sdp_negotiation;
#[path = "sdp-rtx-apt.rs"]
mod sdp_rtx_apt;
#[path = "sdp-unique-mids.rs"]
mod sdp_unique_mids;
#[path = "simulcast-rid-mapping.rs"]
mod simulcast_rid_mapping;
#[path = "srtp-replay-attack.rs"]
mod srtp_replay_attack;
mod ssrc_zero_feedback;
#[path = "stale-network-time-after-pause.rs"]
mod stale_network_time_after_pause;
#[path = "stream-lifecycle.rs"]
mod stream_lifecycle;
mod twcc;
mod unidirectional;
#[path = "unidirectional-r-create-media.rs"]
mod unidirectional_r_create_media;
#[path = "user-rtp-header-extension.rs"]
mod user_rtp_header_extension;
#[path = "writer-borrow.rs"]
mod writer_borrow;
