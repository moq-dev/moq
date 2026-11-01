//! # Google Congestion Control
//!
//! This crate is a Rust port of the [Google Congestion Control](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/) (GCC) implementation from the WebRTC project.
//! No more painful linking against libwebrtc.
//!
//! Google Congestion Control is the congestion control algorithm used in WebRTC.
//! It is a hybrid algorithm that combines the strengths of delay-based and loss-based congestion control.
//! The end goal is minimum latency and to avoid buffer-bloat as much as possible.
//!
//! All licensing from the original source code is preserved under a BSD-style license.
//! Thanks to the WebRTC project authors for their hard work on this implementation.
//! All I did was port it to Rust.
//!
//! ## Notable Changes
//! **NetworkStateEstimator and NetworkStatePredictor have been removed.**
//! They are not implemented in the WebRTC source code and are flagged with a "subject to change" comment.
//! I think this is a back-door for Google to tweak the congestion control algorithm?
//!
//! **FieldTrials is a type-safe struct instead of a string.**
//! The original implementation uses a string to represent field trials.
//! This was quite error-prone as many trials do not match the cooresponding field name.
//! I decided to create a [FieldTrials](experiments::FieldTrials) struct to represent field trials in a type-safe way.
//!
//! **RtcEventLog has been removed.**
//! The structured logs are cool, but have been removed for now to simplify the code.
//!
//! **RTC_LOG has been replaced with [tracing].**
//! Use [tracing_subscriber](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/index.html) to enable and configure the logger.

mod api;

/// [FieldTrials](experiments::FieldTrials) and dynamic configuration.
pub mod experiments;

// We inline goog_cc modules to avoid goog_cc::goog_cc logging prefix.
mod pacing;
mod remote_bitrate_estimator;
mod acknowledged_bitrate_estimator;
mod acknowledged_bitrate_estimator_interface;
mod alr_detector;
mod bitrate_estimator;
mod congestion_window_pushback_controller;
mod delay_based_bwe;
mod delay_increase_detector_interface;
mod goog_cc_network_control;
mod inter_arrival_delta;
mod link_capacity_estimator;
mod loss_based_bandwidth_estimation;
mod loss_based_bwe_v2;
mod probe_bitrate_estimator;
mod probe_controller;
mod robust_throughput_estimator;
mod send_side_bandwidth_estimation;
mod trendline_estimator;

use acknowledged_bitrate_estimator::*;
use acknowledged_bitrate_estimator_interface::*;
use alr_detector::*;
use bitrate_estimator::*;
use congestion_window_pushback_controller::*;
use delay_based_bwe::*;
use delay_increase_detector_interface::*;
use inter_arrival_delta::*;
use link_capacity_estimator::*;
use loss_based_bandwidth_estimation::*;
use loss_based_bwe_v2::*;
use probe_bitrate_estimator::*;
use probe_controller::*;
use robust_throughput_estimator::*;
use send_side_bandwidth_estimation::*;
use trendline_estimator::*;

// Selectively export the public API.
pub use api::{network_control, transport, units};
pub use goog_cc_network_control::{GoogCcConfig, GoogCcNetworkController};
pub use {
    alr_detector::AlrDetectorConfig, bitrate_estimator::BitrateEstimatorConfig,
    send_side_bandwidth_estimation::BweLossExperiment,
    delay_based_bwe::BweSeparateAudioPacketsSettings,
    loss_based_bwe_v2::LossBasedBweV2Config,
    loss_based_bandwidth_estimation::LossBasedControlConfig,
    probe_controller::ProbeControllerConfig,
    robust_throughput_estimator::RobustThroughputEstimatorSettings,
    send_side_bandwidth_estimation::RttBasedBackoffConfig, goog_cc_network_control::SafeResetOnRouteChange, trendline_estimator::TrendlineEstimatorSettings,
};
