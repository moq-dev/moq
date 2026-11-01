/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::experiments::FieldTrials;

use super::{transport::*, units::TimeDelta};

/*
pub trait TargetTransferRateObserver {
    /// Called to indicate target transfer rate as well as giving information about
    /// the current estimate of network parameters.
    fn on_target_transfer_rate(target_transfer_rate: TargetTransferRate);
    /// Called to provide updates to the expected target rate in case it changes
    /// before the first call to OnTargetTransferRate.
    fn on_start_rate_update(data_rate: DataRate);
}
*/

/// Configuration sent to factory create function. The parameters here are
/// optional to use for a network controller implementation.
#[derive(Default, Debug, Clone)]
pub struct NetworkControllerConfig {
    pub field_trials: FieldTrials,

    /// The initial constraints to start with, these can be changed at any later
    /// time by calls to OnTargetRateConstraints. Note that the starting rate
    /// has to be set initially to provide a starting state for the network
    /// controller, even though the field is marked as optional.
    pub constraints: TargetRateConstraints,
    /// Initial stream specific configuration, these are changed at any later time
    /// by calls to OnStreamsConfig.
    pub stream_based_config: StreamsConfig,
}

/// NetworkControllerInterface is implemented by network controllers. A network
/// controller is a class that uses information about network state and traffic
/// to estimate network parameters such as round trip time and bandwidth. Network
/// controllers does not guarantee thread safety, the interface must be used in a
/// non-concurrent fashion.
pub trait NetworkControllerInterface {
    /// Called when network availabilty changes.
    fn on_network_availability(&mut self, msg: NetworkAvailability) -> NetworkControlUpdate;
    /// Called when the receiving or sending endpoint changes address.
    fn on_network_route_change(&mut self, msg: NetworkRouteChange) -> NetworkControlUpdate;
    /// Called periodically with a periodicy as specified by
    /// NetworkControllerFactoryInterface::GetProcessInterval.
    fn on_process_interval(&mut self, msg: ProcessInterval) -> NetworkControlUpdate;
    /// Called when remotely calculated bitrate is received.
    fn on_remote_bitrate_report(&mut self, msg: RemoteBitrateReport) -> NetworkControlUpdate;
    /// Called round trip time has been calculated by protocol specific mechanisms.
    fn on_round_trip_time_update(&mut self, msg: RoundTripTimeUpdate) -> NetworkControlUpdate;
    /// Called when a packet is sent on the network.
    fn on_sent_packet(&mut self, msg: SentPacket) -> NetworkControlUpdate;
    /// Called when a packet is received from the remote client.
    fn on_received_packet(&mut self, msg: ReceivedPacket) -> NetworkControlUpdate;
    /// Called when the stream specific configuration has been updated.
    fn on_streams_config(&mut self, msg: StreamsConfig) -> NetworkControlUpdate;
    /// Called when target transfer rate constraints has been changed.
    fn on_target_rate_constraints(&mut self, msg: TargetRateConstraints) -> NetworkControlUpdate;
    /// Called when a protocol specific calculation of packet loss has been made.
    fn on_transport_loss_report(&mut self, msg: TransportLossReport) -> NetworkControlUpdate;
    /// Called with per packet feedback regarding receive time.
    fn on_transport_packets_feedback(
        &mut self,
        msg: TransportPacketsFeedback,
    ) -> NetworkControlUpdate;
    /// Called with network state estimate updates.
    fn on_network_state_estimate(&mut self, msg: NetworkStateEstimate) -> NetworkControlUpdate;

    /// Returns the interval by which the network controller expects
    /// OnProcessInterval calls.
    fn get_process_interval() -> TimeDelta;
}

/*
// NetworkControllerFactoryInterface is an interface for creating a network
// controller.
pub trait NetworkControllerFactoryInterface {
    // Used to create a new network controller, requires an observer to be
    // provided to handle callbacks.
    fn create(config: NetworkControllerConfig) -> impl NetworkControllerInterface;
}
*/

// Under development, subject to change without notice.
/* Purposely removed because nothing implements it (except for secret Google stuff?)
pub trait NetworkStateEstimator {
    // Gets the current best estimate according to the estimator.
    fn GetCurrentEstimate(&self) -> Option<NetworkStateEstimate>;
    // Called with per packet feedback regarding receive time.
    // Used when the NetworkStateEstimator runs in the sending endpoint.
    fn OnTransportPacketsFeedback(&mut self, event: &TransportPacketsFeedback);
    // Called with per packet feedback regarding receive time.
    // Used when the NetworkStateEstimator runs in the receiving endpoint.
    fn OnReceivedPacket(&mut self, event: &PacketResult);
    // Called when the receiving or sending endpoint changes address.
    fn OnRouteChange(&mut self, event: &NetworkRouteChange);
}

pub trait NetworkStateEstimatorFactory {
    fn Create() -> impl NetworkStateEstimator;
}
*/
