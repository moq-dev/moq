/*
 *  Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::{
    transport::BandwidthUsage,
    units::{DataRate, TimeDelta},
};

pub const CONGESTION_CONTROLLER_MIN_BITRATE: DataRate = DataRate::from_bits_per_sec(5_000);
pub const BITRATE_WINDOW: TimeDelta = TimeDelta::from_seconds(1);

/* unused
pub enum BweNames {
    ReceiverNoExtension = 0,
    ReceiverTOffset = 1,
    ReceiverAbsSendTime = 2,
    SendSideTransportSeqNum = 3,
    BweNamesMax = 4,
}
*/

pub struct RateControlInput {
    pub bw_state: BandwidthUsage,
    pub estimated_throughput: Option<DataRate>,
}

impl RateControlInput {
    pub fn new(bw_state: BandwidthUsage, estimated_throughput: Option<DataRate>) -> Self {
        RateControlInput {
            bw_state,
            estimated_throughput,
        }
    }
}
