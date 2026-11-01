/*
 *  Copyright (c) 2019 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::{
    transport::PacketResult,
    units::{DataRate, Timestamp},
};

pub trait AcknowledgedBitrateEstimatorInterface {
    fn incoming_packet_feedback(&mut self, packet_feedback: &[PacketResult]);
    fn bitrate(&self) -> Option<DataRate>;
    fn peek_rate(&self) -> Option<DataRate>;
    fn set_alr(&mut self, in_alr: bool);
    fn set_alr_ended_time(&mut self, alr_ended_time: Timestamp);
}
