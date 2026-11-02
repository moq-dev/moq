/*
 *  Copyright (c) 2019 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! Upstream's `RateControlSettings` also carries the video encoder settings
//! (`WebRTC-VideoRateControl`), which no congestion controller reads; only the congestion window
//! remains.

use crate::gcc::{api::units::DataSize, experiments::FieldTrials};

/// WebRTC-CongestionWindow
#[derive(Clone, Debug)]
pub struct CongestionWindowConfig {
    pub queue_size_ms: i64,                    // QueueSize
    pub min_bitrate_bps: i64,                  // MinBitrate
    pub initial_data_window: Option<DataSize>, // InitWin
    pub drop_frame_only: bool,                 // DropFrame
}

impl Default for CongestionWindowConfig {
    // QueueSize:350,MinBitrate:30000,DropFrame:true
    fn default() -> Self {
        Self {
            queue_size_ms: 350,
            min_bitrate_bps: 30000,
            initial_data_window: None,
            drop_frame_only: true,
        }
    }
}

#[derive(Clone, Default)]
pub struct RateControlSettings {
    congestion_window_config: CongestionWindowConfig,
}

impl RateControlSettings {
    pub fn new(field_trials: &FieldTrials) -> Self {
        Self {
            congestion_window_config: field_trials.congestion_window.clone(),
        }
    }

    /// When CongestionWindowPushback is enabled, the pacer is oblivious to
    /// the congestion window. The relation between outstanding data and
    /// the congestion window affects encoder allocations directly.
    pub fn use_congestion_window(&self) -> bool {
        self.congestion_window_config.queue_size_ms > 0
    }

    pub fn get_congestion_window_additional_time_ms(&self) -> i64 {
        self.congestion_window_config.queue_size_ms
    }

    pub fn use_congestion_window_pushback(&self) -> bool {
        self.congestion_window_config.queue_size_ms > 0
            && self.congestion_window_config.min_bitrate_bps > 0
    }

    pub fn use_congestion_window_drop_frame_only(&self) -> bool {
        self.congestion_window_config.drop_frame_only
    }

    pub fn congestion_window_min_pushback_target_bitrate_bps(&self) -> u32 {
        self.congestion_window_config.min_bitrate_bps as _
    }

    pub fn congestion_window_initial_data_window(&self) -> Option<DataSize> {
        self.congestion_window_config.initial_data_window
    }
}
