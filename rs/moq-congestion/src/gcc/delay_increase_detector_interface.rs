/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::transport::BandwidthUsage;

pub trait DelayIncreaseDetectorInterface {
    // Update the detector with a new sample. The deltas should represent deltas
    // between timestamp groups as defined by the InterArrival class.
    fn update(
        &mut self,
        recv_delta_ms: f64,
        send_delta_ms: f64,
        send_time_ms: i64,
        arrival_time_ms: i64,
        packet_size: usize,
        calculated_deltas: bool,
    );

    fn state(&self) -> BandwidthUsage;
}
