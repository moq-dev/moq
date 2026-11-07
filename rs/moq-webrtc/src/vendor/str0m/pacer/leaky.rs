use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::Pacer;
use super::PaddingRequest;
use super::QueueState;
use crate::Reason;
use crate::bwe_::ProbeClusterConfig;
use crate::bwe_::ProbeClusterState;
use crate::bwe_::{log_pacer_media_debt, log_pacer_padding_debt};
use crate::pacer::PacerReason;
use crate::rtp_::{Bitrate, DataSize, MidRid, TwccClusterId};
use crate::util::Soonest;

const MAX_BITRATE: Bitrate = Bitrate::gbps(10);
const MAX_DEBT_IN_TIME: Duration = Duration::from_millis(500);
const PADDING_BURST_INTERVAL: Duration = Duration::from_millis(5);
const PACING: Duration = Duration::from_millis(40);
/// Max size of a burst of paced media packets, to not risk overfilling socket buffers at
/// high bitrates. Same as libwebrtc's `PacingController::kMaxBurstSize`.
const MAX_BURST_SIZE: DataSize = DataSize::bytes(63_000);

/// A leaky bucket pacer that can overshoot the target bitrate when required.
pub struct LeakyBucketPacer {
    /// Pacing bitrate.
    pacing_bitrate: Bitrate,
    /// Adjusted pacing bitrate for when we need to drain queues.
    adjusted_bitrate: Bitrate,
    /// The bitrate at which to send padding packets when the pacing rate isn't being achieved.
    padding_bitrate: Bitrate,
    /// The last time we refreshed media debt and potentially adjusted the bitrate.
    last_handle_time: Option<Instant>,
    /// The last time we indicated that a packet should be sent.
    last_emitted: Option<Instant>,
    /// The next time we should send a queued packet.
    next_poll_time: Option<(Instant, PacerReason)>,
    /// The current media debt.
    media_debt: DataSize,
    /// The current padding debt.
    padding_debt: DataSize,
    /// The longest the average packet can spend in the queue before we force it to be drained.
    queue_limit: Duration,
    /// The queue states given by last handle_timeout.
    queue_states: Vec<QueueState>,
    /// The next return value for `poll_queue``
    next_poll_queue: Option<MidRid>,
    /// Queue of probe clusters waiting to be executed
    probe_queue: VecDeque<ProbeClusterState>,
    /// Last completed probe cluster (to be consumed by check_probe_complete)
    completed_probe: Option<TwccClusterId>,
    /// Gates poll_queue() until handle_timeout() is called after packet emission.
    needs_timeout_before_next_poll: bool,
}

impl Pacer for LeakyBucketPacer {
    fn set_pacing_rate(&mut self, pacing_bitrate: Bitrate) {
        self.pacing_bitrate = pacing_bitrate;

        // bitrate will be updated on next handle_timeout().
    }

    fn set_padding_rate(&mut self, padding_bitrate: Bitrate) {
        self.padding_bitrate = padding_bitrate;

        // bitrate will be updated on next handle_timeout().
    }

    fn poll_timeout(&self) -> (Option<Instant>, Reason) {
        let next_handle_time = self.last_handle_time.map(|lh| lh + PACING);

        let poll_at = self
            .next_poll_time
            .map(|(t, r)| (Some(t), Reason::Pacer(r)))
            .unwrap_or((None, Reason::NotHappening));

        (next_handle_time, Reason::Pacer(PacerReason::Handle)).soonest(poll_at)
    }

    fn handle_timeout(
        &mut self,
        now: Instant,
        iter: impl Iterator<Item = QueueState>,
    ) -> Option<PaddingRequest> {
        // Clear the gate when time advances
        self.needs_timeout_before_next_poll = false;

        // Clear the poll time - it will be recalculated below if needed.
        // This is important because if we return early (e.g., next_poll_queue is already set),
        // we don't want the old Immediate timeout to keep firing.
        self.next_poll_time = None;

        // This is called periodically and whenever packet is queued.
        self.queue_states.clear();
        self.queue_states.extend(iter);

        let elapsed = self.update_handle_time_and_get_elapsed(now);

        self.clear_debt(elapsed);
        self.maybe_update_adjusted_bitrate(now);

        if let Some(request) = self.maybe_create_padding_request(now) {
            self.next_poll_queue = Some(request.midrid);
            return Some(request);
        }

        if self.next_poll_queue.is_some() {
            return None;
        }

        let (next_poll_time_and_reason, queue) = self.next_poll(now)?;

        if now < next_poll_time_and_reason.0 {
            // We don't set this because between now and the neaxt poll, queue state can change such
            // that we should poll a different queue i.e. media could be queued.
            self.next_poll_queue = None;
        } else {
            self.next_poll_queue = queue.map(|q| q.midrid);
        }

        self.next_poll_time = Some(next_poll_time_and_reason);

        None
    }

    fn poll_queue(&mut self) -> Option<(MidRid, Option<TwccClusterId>)> {
        // GATE: Block if we need timeout first
        if self.needs_timeout_before_next_poll {
            return None;
        }

        let next = self.next_poll_queue.take()?;

        // Mark that we need timeout before next poll
        self.needs_timeout_before_next_poll = true;
        self.request_immediate_timeout();

        // Capture the cluster ID at poll time, before register_send() might clear it
        let cluster_id = self.active_cluster();

        Some((next, cluster_id))
    }

    fn register_send(&mut self, now: Instant, packet_size: DataSize, _from: MidRid) {
        self.last_emitted = Some(now);

        self.media_debt += packet_size;
        self.media_debt = self
            .media_debt
            .min(self.adjusted_bitrate * MAX_DEBT_IN_TIME);
        log_pacer_media_debt!(self.media_debt.as_bytes_usize());
        self.add_padding_debt(packet_size);

        // Update active probe state to track this packet
        // This ensures probe timing advances correctly even when sending media packets
        if let Some(probe) = self.probe_queue.front_mut() {
            probe.record_packet(now, packet_size);
        }

        // Check if probe is complete and store it for later retrieval
        if let Some(cluster_id) = self.check_probe_complete_internal(now) {
            self.completed_probe = Some(cluster_id);
        }
    }
}

impl LeakyBucketPacer {
    pub fn new(initial_pacing_bitrate: Bitrate) -> Self {
        const DEFAULT_QUEUE_LIMIT: Duration = Duration::from_secs(2);

        Self {
            pacing_bitrate: initial_pacing_bitrate,
            adjusted_bitrate: Bitrate::ZERO,
            padding_bitrate: Bitrate::ZERO,
            last_handle_time: None,
            last_emitted: None,
            next_poll_time: None,
            media_debt: DataSize::ZERO,
            padding_debt: DataSize::ZERO,
            queue_limit: DEFAULT_QUEUE_LIMIT,
            queue_states: vec![],
            next_poll_queue: None,
            probe_queue: VecDeque::new(),
            completed_probe: None,
            needs_timeout_before_next_poll: true,
        }
    }

    /// Start executing a probe cluster.
    ///
    /// The pacer will pace at the probe's target bitrate and track packets sent.
    /// Probes are queued and executed sequentially.
    pub(crate) fn start_probe(&mut self, config: ProbeClusterConfig) {
        trace!(?config, "Probe start");
        self.probe_queue.push_back(ProbeClusterState::new(config));
        self.request_immediate_timeout();
    }

    /// Get the cluster ID of the active probe, if any.
    pub(crate) fn active_cluster(&self) -> Option<TwccClusterId> {
        self.probe_queue.front().map(|p| p.config().cluster())
    }

    /// Check if the active probe is complete and should be finished.
    pub(crate) fn check_probe_complete(&mut self, now: Instant) -> Option<TwccClusterId> {
        // Check if we have a completed probe from a previous call
        if let Some(cluster_id) = self.completed_probe.take() {
            return Some(cluster_id);
        }

        // Otherwise check if the active probe just completed
        self.check_probe_complete_internal(now)
    }

    /// Internal method to check if probe is complete (doesn't consume completed_probe)
    fn check_probe_complete_internal(&mut self, now: Instant) -> Option<TwccClusterId> {
        let probe = self.probe_queue.front()?;

        if probe.is_complete(now) {
            let cluster_id = probe.config().cluster();
            self.probe_queue.pop_front();
            return Some(cluster_id);
        }

        None
    }

    fn update_handle_time_and_get_elapsed(&mut self, now: Instant) -> Duration {
        // Due the calling code this also happens when a packet is queued in any upstream queue.
        let Some(previous_handle_time) = self.last_handle_time else {
            self.last_handle_time = Some(now);
            return Duration::ZERO;
        };

        let elapsed = now - previous_handle_time;
        self.last_handle_time = Some(now);

        elapsed
    }

    fn clear_debt(&mut self, elapsed: Duration) {
        self.media_debt = self
            .media_debt
            .saturating_sub(self.adjusted_bitrate * elapsed);
        self.padding_debt = self
            .padding_debt
            .saturating_sub(self.padding_bitrate * elapsed);
        log_pacer_media_debt!(self.media_debt.as_bytes_usize());
        log_pacer_padding_debt!(self.padding_debt.as_bytes_usize());
    }

    fn next_poll(&self, now: Instant) -> Option<((Instant, PacerReason), Option<&QueueState>)> {
        // If we have never sent before, do so immediately on an arbitrary non-empty queue.
        if self.last_emitted.is_none() {
            let mut queues = self
                .queue_states
                .iter()
                .filter(|q| q.snapshot.packet_count > 0);

            if let Some(queue) = queues.next() {
                return Some(((now, PacerReason::FirstEver), Some(queue)));
            }
        };

        let unpaced = self
            .queue_states
            .iter()
            .filter(|qs| qs.unpaced)
            .filter_map(|qs| qs.snapshot.first_unsent.map(|t| (t, qs)))
            .min_by_key(|(t, _)| *t);

        // Unpaced packets (such as audio by default) are sent immediately.
        if let Some((queued_at, qs)) = unpaced {
            return Some(((queued_at, PacerReason::Unpaced), Some(qs)));
        }

        let non_empty_queue = {
            let non_empty_queues = self
                .queue_states
                .iter()
                .filter(|q| q.snapshot.packet_count > 0);

            // Send on the non-empty queue with the lowest priority that, was least recently
            // sent on.
            non_empty_queues.min_by_key(|q| (q.snapshot.priority, q.snapshot.last_emitted))
        };

        if let Some(queue) = non_empty_queue {
            if self.adjusted_bitrate > Bitrate::ZERO {
                // Check if we're actively probing and should use probe-specific timing
                let poll_at = if let Some(probe) = self.probe_queue.front() {
                    // During probe: use absolute time directly from probe state
                    (probe.next_probe_time(), PacerReason::Probe1)
                } else {
                    // Normal pacing: use relative offset based on debt
                    let drain_debt_time = self.media_debt / self.adjusted_bitrate;
                    // Limit the burst to MAX_BURST_SIZE so high bitrates don't overfill socket
                    // buffers. Below ~12.6 Mbps this is PACING.
                    let burst_interval = PACING.min(MAX_BURST_SIZE / self.adjusted_bitrate);
                    let next_send_offset = if drain_debt_time >= burst_interval {
                        // If we have incurred too much debt, wait for it to clear before sending.
                        // again.
                        drain_debt_time
                    } else {
                        Duration::ZERO
                    };

                    let time = self
                        .last_handle_time
                        .map(|h| h + next_send_offset)
                        .unwrap_or(now);

                    (time, PacerReason::Paced)
                };

                return Some((poll_at, Some(queue)));
            }
        }

        let any_queue_for_padding = self.queue_states.iter().any(|q| q.use_for_padding);
        if !any_queue_for_padding {
            return None;
        }

        // If we're actively probing, use probe timing for padding
        if let Some(probe) = self.probe_queue.front() {
            let next_probe_time = probe.next_probe_time();
            // We explicitly don't return a queue to poll here. We need another call to
            // handle_timeout to request the padding before we can poll the selected queue.
            return Some(((next_probe_time, PacerReason::Probe2), None));
        }

        if self.padding_bitrate == Bitrate::ZERO {
            return None;
        }

        // If all queues are empty and we have a padding rate, wait until we have drained
        // both the media debt and padding debt to send some padding.
        let mut drain_debt_time =
            (self.media_debt / self.adjusted_bitrate).max(self.padding_debt / self.padding_bitrate);
        if drain_debt_time.is_zero() {
            // Give the main loop some time to do something else e.g. queue media.
            drain_debt_time = Duration::from_micros(1);
        }

        let padding_at = self
            .last_handle_time
            .map(|h| h + drain_debt_time)
            .unwrap_or(now);

        // We explicitly don't return a queue to poll here. We need another call to
        // handle_timeout to request the padding before we can poll the selected queue.
        Some(((padding_at, PacerReason::Padding), None))
    }

    fn maybe_update_adjusted_bitrate(&mut self, now: Instant) {
        // Use probe's target bitrate if actively probing, otherwise use pacing bitrate
        self.adjusted_bitrate = if let Some(probe) = self.probe_queue.front() {
            probe.config().target_bitrate()
        } else {
            self.pacing_bitrate
        };

        let (queue_time, queued_packets, queue_size) =
            self.queue_states
                .iter()
                .fold((Duration::ZERO, 0, DataSize::ZERO), |acc, q| {
                    (
                        acc.0 + q.snapshot.total_queue_time(now),
                        acc.1 + q.snapshot.packet_count,
                        acc.2 + DataSize::from(q.snapshot.byte_size),
                    )
                });
        if queued_packets == 0 {
            return;
        }

        let avg_queue_time = queue_time / queued_packets;

        // The average time we want the packet in the queue to at most to wait to drain.
        let target_queue_wait =
            Duration::from_millis(1).max(self.queue_limit.saturating_sub(avg_queue_time));
        // Min data rate to drain what's currently in the queue.
        let min_rate = queue_size / target_queue_wait;
        if min_rate > self.adjusted_bitrate {
            // Min rate exceeds our pacing rate, increase the rate to force drain the queue.
            self.adjusted_bitrate = min_rate.clamp(Bitrate::ZERO, MAX_BITRATE);
        }
    }

    fn add_padding_debt(&mut self, size: DataSize) {
        self.padding_debt += size;
        self.padding_debt = self
            .padding_debt
            .min(self.padding_bitrate * MAX_DEBT_IN_TIME);
        log_pacer_padding_debt!(self.padding_debt.as_bytes_usize());
    }

    /// Optimistically attempt to create a padding request.
    ///
    /// Returns `Some(PaddingRequest)` if padding is enabled and the current queue state
    /// allows padding, otherwise returns `None`.
    fn maybe_create_padding_request(&mut self, now: Instant) -> Option<PaddingRequest> {
        // Queues must be empty.
        let all_queues_empty = self
            .queue_states
            .iter()
            .all(|q| q.snapshot.packet_count == 0);
        if !all_queues_empty {
            return None;
        }

        // We must have a queue that supports padding.
        let maybe_queue = self
            .queue_states
            .iter()
            .filter(|q| q.use_for_padding)
            .max_by_key(|q| q.snapshot.last_emitted);

        if maybe_queue.is_none() {
            // No padding queue, no probes.
            self.probe_queue.clear();
        }

        let queue = maybe_queue?;

        // Check for PROBE padding FIRST (bypasses debt checks)
        // Active probes need padding to hit their target bitrate when there's insufficient media.
        if let Some(probe) = self.probe_queue.front_mut() {
            // Delegate probe timing and padding calculation to ProbeClusterState
            if !probe.should_send_now(now) {
                // Not time yet - wait until next_probe_time
                return None;
            }

            // Get recommended padding amount from ProbeClusterState
            // This handles the calculation of how much padding is needed based on probe timing
            let padding_size = probe.next_packet(now);
            let Some(padding_size) = padding_size else {
                // Probe says no padding needed (already sent enough for this interval)
                return None;
            };

            return Some(PaddingRequest {
                midrid: queue.midrid,
                padding: padding_size.as_bytes_usize(),
            });
        }

        // Normal padding: requires zero debt
        if self.media_debt != DataSize::ZERO || self.padding_debt != DataSize::ZERO {
            return None;
        }

        if self.padding_bitrate == Bitrate::ZERO {
            return None;
        }

        // We can generate padding
        let padding = (self.padding_bitrate * PADDING_BURST_INTERVAL).as_bytes_usize();

        Some(PaddingRequest {
            midrid: queue.midrid,
            padding,
        })
    }

    fn request_immediate_timeout(&mut self) {
        // Request timeout at the next microsecond to ensure time advances between packets.
        // We can't use already_happened() because that would cause the test harness to
        // set a very old timestamp, and while lib.rs prevents last_now from going backwards,
        // it doesn't force it to advance, so all packets would get the same timestamp.
        const MINIMAL_DELTA: Duration = Duration::from_micros(1);

        let Some(time) = self.last_handle_time.map(|t| t + MINIMAL_DELTA) else {
            self.next_poll_time = None;
            return;
        };

        self.next_poll_time = Some((time, PacerReason::Immediate));
    }
}

#[cfg(test)]
mod test {
    use super::super::{QueuePriority, QueueSnapshot};
    use super::*;
    use crate::rtp_::{DataSize, Mid, RtpHeader};
    use queue::{PacketKind, Queue, QueuedPacket};
    use std::time::{Duration, Instant};

    #[test]
    fn probe_deadline_without_media_or_regular_padding() {
        use crate::bwe_::ProbeKind;

        let now = Instant::now();
        let mut pacer = LeakyBucketPacer::new(Bitrate::kbps(300));
        let queue = QueueState {
            midrid: MidRid("probe".into(), None),
            unpaced: false,
            use_for_padding: true,
            snapshot: QueueSnapshot {
                created_at: now,
                ..Default::default()
            },
        };
        pacer.handle_timeout(now, std::iter::once(queue));
        pacer.start_probe(ProbeClusterConfig::new(
            0.into(),
            Bitrate::kbps(300),
            ProbeKind::Initial,
        ));
        let request = pacer.handle_timeout(now, std::iter::once(queue)).unwrap();
        let (midrid, cluster) = pacer.poll_queue().unwrap();
        assert_eq!(cluster, Some(0.into()));
        pacer.register_send(now, request.padding.into(), midrid);
        assert!(pacer.handle_timeout(now, std::iter::once(queue)).is_none());
        let deadline = pacer.poll_timeout().0.unwrap();
        assert!(deadline > now && deadline <= now + Duration::from_millis(10));
        assert!(pacer.poll_queue().is_none());
        assert!(
            pacer
                .handle_timeout(deadline, std::iter::once(queue))
                .is_some()
        );
    }

    #[test]
    fn test_typical_behavior() {
        let now = Instant::now();
        let mut queue = Queue::default();
        // 2,000 bits per second, 10 bytes per pacing interval(40ms)
        let mut pacer = LeakyBucketPacer::new((10 * 200).into());
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(1));

        assert!(
            pacer.poll_queue().is_none(),
            "We initially attempt to poll any non-empty queue if we have never sent",
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            1,
            5,
            PacketKind::Video,
            now + duration_ms(21),
        );

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(21),
            "First packet should be released because we have no debt",
            |packet| {
                assert_eq!(packet.header.sequence_number, 1);
            },
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            2,
            8,
            PacketKind::Video,
            now + duration_ms(27),
        );
        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            3,
            25,
            PacketKind::Video,
            now + duration_ms(28),
        );

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(28),
            "Second packet should be released because the debt is within tolerance",
            |packet| {
                assert_eq!(packet.header.sequence_number, 2);
            },
        );

        // We have incurred too much media debt so polling will now fail until the debt can be
        // reduced.
        assert!(
            pacer.poll_queue().is_none(),
            "Third packet should not be released because we have too much debt"
        );

        // Periodic timeout
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(41));

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(41),
            "Third packet should be released because we have cleared debt as time moved forward",
            |packet| {
                assert_eq!(packet.header.sequence_number, 3);
            },
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            4,
            12,
            PacketKind::Video,
            now + duration_ms(45),
        );
        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            5,
            25,
            PacketKind::Video,
            now + duration_ms(47),
        );

        // We have incurred too much media debt so polling will now fail until the debt can be
        // reduced.
        assert!(
            pacer.poll_queue().is_none(),
            "Fourth packet should not be released because we have too much debt"
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            6,
            100,
            PacketKind::Audio,
            now + duration_ms(52),
        );

        // Unpaced packets should be able to send even if we have too much media debt.
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(52),
            "Sixth packet (audio) should be released despite too much media debt because \
            audio packets are not paced",
            |packet| {
                assert_eq!(packet.kind, PacketKind::Audio);
                assert_eq!(packet.header.sequence_number, 6);
            },
        );

        // A lot of time passes, now the bitrate should be adjusted to force drain the queues to
        // avoid packets being queued for too long.
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(2053));

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(2053),
            "Fourth packet should be released after hitting the queue limit",
            |packet| {
                assert_eq!(packet.header.sequence_number, 4);
            },
        );

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(2053),
            "Fifth packet should be released after hitting the queue limit",
            |packet| {
                assert_eq!(packet.header.sequence_number, 5);
            },
        );

        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_drain() {
        let now = Instant::now();
        let mut queue = Queue::default();
        // 2,000 bits per second, 10 bytes per pacing interval(40ms)
        let mut pacer = LeakyBucketPacer::new((10 * 200).into());
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(1));

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            1,
            21,
            PacketKind::Video,
            now + duration_ms(21),
        );

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(21),
            "First packet should be released because we have no debt",
            |packet| {
                assert_eq!(packet.header.sequence_number, 1);
            },
        );

        // Time moves forward
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(41));

        // Nothing happens for a while because there's nothing in the queues.

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            2,
            8,
            PacketKind::Video,
            // Debt will be just slightly above what can be drained in 40 ms
            // after 66ms
            now + duration_ms(66),
        );

        assert!(
            pacer.poll_queue().is_none(),
            "Second packet should not be released because there's too much debt"
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            3,
            5,
            PacketKind::Video,
            now + duration_ms(70),
        );
        // Drain packet 2
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(70),
            "Second packet should be released because of the adjusted bitrate to drain the queue",
            |packet| {
                assert_eq!(packet.header.sequence_number, 2);
            },
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            4,
            1200,
            PacketKind::Video,
            now + duration_ms(71),
        );

        // Drain packet 3
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(71),
            "Third packet should be released because of the adjusted bitrate to drain the queue",
            |packet| {
                assert_eq!(packet.header.sequence_number, 3);
            },
        );

        // Drain packet 4
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(71),
            "Fourth packet should be released because of the adjusted bitrate to drain the queue",
            |packet| {
                assert_eq!(packet.header.sequence_number, 4);
            },
        );

        // Time moves forward
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(81));

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            5,
            40,
            PacketKind::Video,
            now + duration_ms(81),
        );

        assert!(
            pacer.poll_queue().is_none(),
            "Fifth packet shoud not be relaesed because there's too much debt"
        );
    }

    #[test]
    fn test_padding_fill_in() {
        let now = Instant::now();
        let mut queue = Queue::default();
        let mut pacer = LeakyBucketPacer::new((10 * 200).into());
        // 2,000 bits per second, 10 bytes per pacing interval(40ms) with padding at 3,000 bits per
        // second, 15 bytes per pacing interval(40ms)
        pacer.set_pacing_rate((10 * 200).into());
        pacer.set_padding_rate((15 * 200).into());
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(1));

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            1,
            21,
            PacketKind::Video,
            now + duration_ms(21),
        );

        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(21),
            "First packet should be released because we have no debt",
            |packet| {
                assert_eq!(packet.header.sequence_number, 1);
            },
        );

        // Time moves forward
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(41));

        // Nothing happens for a while because there's nothing in the queues.

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            2,
            8,
            PacketKind::Video,
            now + duration_ms(70),
        );

        // Drain packet 2
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(70),
            "Second packet should be released because of the adjusted bitrate to drain the queue",
            |packet| {
                assert_eq!(packet.header.sequence_number, 2);
            },
        );

        // Time moves forward, all debt is cleared out now
        handle_timeout_noisy(&mut pacer, &mut queue, now + duration_ms(155));

        // Drain padding packet
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(165),
            "The queued padding packet should be drained",
            |packet| {
                assert_eq!(packet.size(), 2);
                assert_eq!(packet.header.sequence_number, 0);
            },
        );

        enqueue_packet_noisy(
            &mut pacer,
            &mut queue,
            3,
            15,
            PacketKind::Video,
            now + duration_ms(165),
        );

        // Drain packet 3
        assert_poll_success(
            &mut pacer,
            &mut queue,
            now + duration_ms(165),
            "Third packet should be released because the sent padding doesn't \
            increase the media debt too much",
            |packet| {
                assert_eq!(packet.header.sequence_number, 3);
            },
        );
    }

    #[test]
    fn test_realistic() {
        let config = RealisticTestConfig {
            padding_rate: Bitrate::kbps(2500),
            max_overshoot_factor: 0.05,
            spike_probability: 3,
            ..Default::default()
        };
        let (media_rate, padding_rate, total_rate) = run_realistic_test(config);
        let expected_padding = config.padding_rate - config.media_rate;
        // Expect result to be within 2 standard deviations.
        let upper_bound =
            config.media_rate + expected_padding * (1.0 + config.max_overshoot_factor * 2.0) as f64;
        let lower_bound =
            config.media_rate + expected_padding * (1.0 - config.max_overshoot_factor * 2.0) as f64;

        assert!(
            total_rate >= lower_bound && total_rate <= upper_bound,
            "Expected reuslting total rate to be within expected bounds. \
            total_rate={total_rate}, media_rate={media_rate}, padding_rate={padding_rate}, \
            config={config:?}, lower_bound={lower_bound}, upper_bound={upper_bound}"
        );
    }

    #[test]
    fn test_queue_state_merge() {
        let now = Instant::now();

        let mut state = QueueState {
            midrid: MidRid(Mid::from("001"), None),
            unpaced: false,
            use_for_padding: true,
            snapshot: QueueSnapshot {
                created_at: now,
                byte_size: 10_usize,
                packet_count: 1332,
                total_queue_time_origin: duration_ms(1_000),
                last_emitted: Some(now + duration_ms(500)),
                first_unsent: None,
                priority: QueuePriority::Media,
            },
        };

        let other = QueueState {
            midrid: MidRid(Mid::from("002"), None),
            unpaced: false,
            use_for_padding: false,
            snapshot: QueueSnapshot {
                created_at: now,
                byte_size: 30_usize,
                packet_count: 5,
                total_queue_time_origin: duration_ms(337),
                last_emitted: None,
                first_unsent: Some(now + duration_ms(19)),
                priority: QueuePriority::Padding,
            },
        };

        state.snapshot.merge(&other.snapshot);

        assert_eq!(state.midrid.mid(), Mid::from("001"));
        assert_eq!(state.snapshot.byte_size, 40_usize);
        assert_eq!(state.snapshot.packet_count, 1337);
        assert_eq!(state.snapshot.total_queue_time_origin, duration_ms(1337));

        assert_eq!(state.snapshot.last_emitted, Some(now + duration_ms(500)));
        assert_eq!(state.snapshot.first_unsent, Some(now + duration_ms(19)));
        assert_eq!(state.snapshot.priority, QueuePriority::Media);
    }

    #[test]
    fn test_max_burst_size_at_high_bitrate() {
        // At 50 Mbps a full PACING interval is 250 kB, which is well above MAX_BURST_SIZE.
        let (burst, next_poll) = measure_burst(Bitrate::mbps(50), 1200, 500);

        assert!(
            burst <= MAX_BURST_SIZE + DataSize::bytes(1200),
            "Burst of {burst} should not exceed MAX_BURST_SIZE by more than one packet"
        );
        assert!(
            burst > MAX_BURST_SIZE,
            "Burst of {burst} should fill up to MAX_BURST_SIZE"
        );
        // 63 kB at 50 Mbps drains in ~10 ms.
        assert!(
            next_poll >= duration_ms(10) && next_poll < PACING,
            "Pacer should wait for the burst to drain, got {next_poll:?}"
        );

        // When packet sizes divide the limit exactly, stop at the limit rather than
        // sending another packet.
        let (burst, _) = measure_burst(Bitrate::mbps(50), 1000, 500);
        assert_eq!(burst, MAX_BURST_SIZE);
    }

    #[test]
    fn test_burst_size_unchanged_at_low_bitrate() {
        // At 10 Mbps a PACING interval is 50 kB, below MAX_BURST_SIZE, so PACING still governs
        // the burst: debt may grow to 50 kB, then the pacer waits.
        let (burst, next_poll) = measure_burst(Bitrate::mbps(10), 1000, 500);
        assert_eq!(burst, DataSize::bytes(50_000));
        // Nothing is sent before the regular PACING tick.
        assert_eq!(next_poll, PACING);

        // At 1 Mbps a PACING interval is 5 kB. Queue less here so the queue drain logic
        // doesn't raise the adjusted bitrate.
        let (burst, next_poll) = measure_burst(Bitrate::mbps(1), 1000, 50);
        assert_eq!(burst, DataSize::bytes(5_000));
        // Nothing is sent before the regular PACING tick.
        assert_eq!(next_poll, PACING);
    }

    /// Queue `count` video packets of `size` bytes at once and poll the pacer until it
    /// stops releasing packets without time moving forward.
    ///
    /// Returns the number of bytes released in that burst and how long after the burst the
    /// pacer wants to be polled again.
    fn measure_burst(bitrate: Bitrate, size: usize, count: u16) -> (DataSize, Duration) {
        let now = Instant::now();
        let mut queue = Queue::default();
        let mut pacer = LeakyBucketPacer::new(bitrate);
        handle_timeout_noisy(&mut pacer, &mut queue, now);

        let at = now + duration_ms(1);
        for seq_no in 0..count {
            enqueue_packet_noisy(&mut pacer, &mut queue, seq_no, size, PacketKind::Video, at);
        }

        let mut burst = DataSize::ZERO;
        while let Some((qid, _)) = pacer.poll_queue() {
            let packet = queue.next_packet().unwrap();
            pacer.register_send(at, DataSize::from(packet.size()), qid);
            queue.register_send(qid, at);
            handle_timeout_noisy(&mut pacer, &mut queue, at);
            burst += DataSize::from(packet.size());
        }

        assert!(!queue.is_empty(), "The burst should be cut short by pacing");

        let next_poll = pacer.poll_timeout().0.unwrap();
        (burst, next_poll - at)
    }

    #[test]
    fn test_priority_ordering() {
        assert!(QueuePriority::Media < QueuePriority::Padding);
        assert!(QueuePriority::Media < QueuePriority::Empty);
        assert!(QueuePriority::Padding < QueuePriority::Empty);
    }

    fn assert_poll_success<F>(
        pacer: &mut impl Pacer,
        queue: &mut Queue,
        now: Instant,
        msg: &str,
        do_asserts: F,
    ) -> Instant
    where
        F: Fn(QueuedPacket),
    {
        let (qid, _cluster_id) = pacer.poll_queue().expect(msg);
        let packet = queue.next_packet().unwrap();
        let packet_size = packet.size();
        do_asserts(packet);
        pacer.register_send(now, DataSize::from(packet_size), qid);
        queue.register_send(qid, now);

        let timeout = pacer.poll_timeout().0;
        // After gating, the pacer requests a timeout at now + 1µs to ensure time advances
        const MINIMAL_DELTA: Duration = Duration::from_micros(1);
        assert!(
            timeout <= Some(now + MINIMAL_DELTA) && timeout.is_some(),
            "After a successful send the pacer should return an immediate timeout"
        );

        // Simulate an immediate call to handle_timeout
        handle_timeout_noisy(pacer, queue, now);

        timeout.unwrap()
    }

    fn enqueue_packet_noisy(
        pacer: &mut impl Pacer,
        queue: &mut Queue,
        seq_no: u16,
        size: usize,
        kind: PacketKind,
        now: Instant,
    ) {
        let (header, payload_len, kind) = make_packet(seq_no, size, kind);

        let queued_packet = QueuedPacket {
            queued_at: now,
            header,
            payload_len,
            kind,
        };
        queue.enqueue_packet(queued_packet);

        // Matches the queueing behavior when the pacer is used in real code.
        // Each packet being queued causes time to move forward in the pacer and the queue.
        handle_timeout_noisy(pacer, queue, now);
    }

    fn handle_timeout_noisy(pacer: &mut impl Pacer, queue: &mut Queue, now: Instant) {
        queue.update_average_queue_time(now);
        if let Some(padding_request) = pacer.handle_timeout(now, queue.queue_state(now)) {
            queue.generate_padding(padding_request.padding, now);

            let timeout = pacer.poll_timeout().0;
            if timeout.map(|t| t <= now).unwrap_or(false) {
                // Refresh queue state
                pacer.handle_timeout(now, queue.queue_state(now));
            }
        }
    }

    fn duration_ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn make_packet(seq_no: u16, size: usize, kind: PacketKind) -> (RtpHeader, usize, PacketKind) {
        let header = RtpHeader {
            sequence_number: seq_no,
            ..Default::default()
        };

        (header, size, kind)
    }

    #[derive(Debug, Clone, Copy)]
    struct RealisticTestConfig {
        media_rate: Bitrate,
        padding_rate: Bitrate,
        duration: Duration,
        // Spike probability as a percentage
        spike_probability: u8,
        max_overshoot_factor: f32,
        frame_pacing: Duration,
    }

    impl Default for RealisticTestConfig {
        fn default() -> Self {
            RealisticTestConfig {
                media_rate: Bitrate::kbps(250),
                padding_rate: Bitrate::kbps(800),
                duration: Duration::from_secs(10),
                spike_probability: 0,
                max_overshoot_factor: 0.25,
                frame_pacing: Duration::from_millis(33), // ~30 FPS
            }
        }
    }

    /// Run a realistic test of the pacer with simulated media.
    ///
    /// Returns the media rate, padding, rate, and total rate achieved by the test.
    fn run_realistic_test(config: RealisticTestConfig) -> (Bitrate, Bitrate, Bitrate) {
        let RealisticTestConfig {
            media_rate,
            padding_rate,
            duration,
            spike_probability,
            max_overshoot_factor,
            frame_pacing,
        } = config;

        let base = Instant::now();
        let mut queue = Queue::default();
        let mut pacer = LeakyBucketPacer::new(media_rate);
        pacer.set_pacing_rate(padding_rate);
        pacer.set_padding_rate(padding_rate);

        let mut last_media_at = base - frame_pacing - Duration::from_millis(1);
        let mut media_sent = DataSize::ZERO;
        let mut padding_sent = DataSize::ZERO;
        let mut elapsed = Duration::ZERO;

        let generate_padding = |queue: &mut Queue, now: Instant, request: PaddingRequest| {
            let rand: f32 = fastrand::f32();
            let overshoot_factor: f32 = rand * max_overshoot_factor;
            let final_size = ((request.padding as f32) * (1.0 + overshoot_factor).round()) as usize;
            queue.generate_padding(final_size, now);
        };

        loop {
            if elapsed > duration {
                break;
            }

            let timeout = {
                if let Some((midrid, _cluster_id)) = pacer.poll_queue() {
                    let packet = queue
                        .next_packet()
                        .unwrap_or_else(|| panic!("Should have a packet for {:?}", midrid));
                    queue.register_send(midrid, base + elapsed);
                    queue.update_average_queue_time(base + elapsed);
                    pacer.register_send(
                        base + elapsed,
                        DataSize::bytes(packet.payload_len as i64),
                        midrid,
                    );
                    if packet.kind == PacketKind::Padding {
                        padding_sent += packet.payload_len.into();
                    } else {
                        media_sent += packet.payload_len.into();
                    }
                    continue;
                }

                pacer.poll_timeout()
            };

            let sleep_until_poll = timeout
                .0
                .map(|t| t.duration_since(base + elapsed))
                .unwrap_or(Duration::ZERO);

            let sleep_until_media =
                frame_pacing.saturating_sub((base + elapsed).duration_since(last_media_at));

            if sleep_until_poll < sleep_until_media {
                elapsed += sleep_until_poll;

                queue.update_average_queue_time(base + elapsed);
                if let Some(padding_request) =
                    pacer.handle_timeout(base + elapsed, queue.queue_state(base + elapsed))
                {
                    generate_padding(&mut queue, base + elapsed, padding_request);
                }
                continue;
            } else {
                elapsed += sleep_until_media;
            }

            let large_overshoot = (fastrand::u8(..) % 100) >= (100 - spike_probability);
            let mut to_add = if large_overshoot {
                (media_rate * 2.5) * frame_pacing
            } else {
                media_rate * frame_pacing
            };

            while to_add > DataSize::ZERO {
                let packet_size = to_add.min(DataSize::bytes(1100));
                let (header, size, kind) =
                    make_packet(0, packet_size.as_bytes_usize(), PacketKind::Video);
                queue.enqueue_packet(QueuedPacket {
                    queued_at: base + elapsed,
                    header,
                    payload_len: size,
                    kind,
                });
                to_add -= packet_size;
            }
            last_media_at = base + elapsed;
        }

        let observed_media_rate = media_sent / duration;
        let observed_padding_rate = padding_sent / duration;
        let total_rate = (media_sent + padding_sent) / duration;

        (observed_media_rate, observed_padding_rate, total_rate)
    }

    /// A packet queue for use in tests of the pacer.
    mod queue {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};

        use crate::rtp_::{DataSize, RtpHeader};

        use super::*;

        // A packet queue
        pub(super) struct Queue {
            /// Queue for audio packets
            audio_queue: Inner,
            /// Queue for video packets
            video_queue: Inner,
            /// Queue for padding packets
            padding_queue: Inner,
        }

        pub(super) struct QueuedPacket {
            pub(super) queued_at: Instant,
            pub(super) header: RtpHeader,
            pub(super) payload_len: usize,
            pub(super) kind: PacketKind,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) enum PacketKind {
            Audio,
            Video,
            Padding,
        }

        impl Queue {
            pub(super) fn is_empty(&self) -> bool {
                self.audio_queue.is_empty() && self.video_queue.is_empty()
            }

            pub(super) fn update_average_queue_time(&mut self, now: Instant) {
                self.audio_queue.update_average_queue_time(now);
                self.video_queue.update_average_queue_time(now);
            }

            pub(super) fn enqueue_packet(&mut self, packet: QueuedPacket) {
                let queue = self.queue_for_kind_mut(packet.kind);
                queue.enqueue(packet);
            }

            pub(super) fn next_packet(&mut self) -> Option<QueuedPacket> {
                if !self.audio_queue.is_empty() {
                    self.audio_queue.pop_packet()
                } else if !self.video_queue.is_empty() {
                    self.video_queue.pop_packet()
                } else {
                    self.padding_queue.pop_packet()
                }
            }

            pub(super) fn queue_state(&self, now: Instant) -> impl Iterator<Item = QueueState> {
                vec![
                    self.audio_queue.queue_state(now),
                    self.video_queue.queue_state(now),
                    self.padding_queue.queue_state(now),
                ]
                .into_iter()
            }

            pub(super) fn register_send(&mut self, midrid: MidRid, now: Instant) {
                if self.video_queue.midrid == midrid {
                    self.video_queue.last_emitted = Some(now);
                } else if self.audio_queue.midrid == midrid {
                    self.audio_queue.last_emitted = Some(now);
                } else if self.padding_queue.midrid == midrid {
                    self.padding_queue.last_emitted = Some(now);
                } else {
                    panic!(
                        "Attempted to register send on unknown queue with id {:?}",
                        midrid
                    );
                }
            }

            pub(super) fn generate_padding(&mut self, mut pad_size: usize, now: Instant) {
                while pad_size > 0 {
                    let final_packet_size = pad_size.min(1200);
                    let final_packet_size = DataSize::bytes(final_packet_size as i64);
                    let (header, payload_len, kind) =
                        make_packet(0, final_packet_size.as_bytes_usize(), PacketKind::Padding);
                    self.enqueue_packet(QueuedPacket {
                        queued_at: now,
                        header,
                        payload_len,
                        kind,
                    });
                    self.update_average_queue_time(now);

                    pad_size = pad_size.saturating_sub(final_packet_size.as_bytes_usize());
                }
            }

            fn queue_for_kind_mut(&mut self, kind: PacketKind) -> &mut Inner {
                match kind {
                    PacketKind::Audio => &mut self.audio_queue,
                    PacketKind::Video => &mut self.video_queue,
                    PacketKind::Padding => &mut self.padding_queue,
                }
            }
        }

        impl Default for Queue {
            fn default() -> Self {
                Self {
                    audio_queue: Inner::new(
                        MidRid(Mid::from("001"), None),
                        true,
                        QueuePriority::Media,
                    ),
                    video_queue: Inner::new(
                        MidRid(Mid::from("002"), None),
                        false,
                        QueuePriority::Media,
                    ),
                    padding_queue: Inner::new(
                        MidRid(Mid::from("003"), None),
                        false,
                        QueuePriority::Padding,
                    ),
                }
            }
        }

        impl QueuedPacket {
            pub(super) fn size(&self) -> usize {
                self.payload_len
            }
        }

        struct Inner {
            midrid: MidRid,
            last_emitted: Option<Instant>,
            queue: VecDeque<QueuedPacket>,
            packet_count: u32,
            total_time_spent_queued: Duration,
            last_update: Option<Instant>,
            is_audio: bool,
            priority: QueuePriority,
        }

        impl Inner {
            fn new(midrid: MidRid, is_audio: bool, priority: QueuePriority) -> Self {
                Self {
                    midrid,
                    last_emitted: None,
                    queue: VecDeque::default(),
                    packet_count: 0,
                    total_time_spent_queued: Duration::ZERO,
                    last_update: None,
                    is_audio,
                    priority,
                }
            }

            fn enqueue(&mut self, packet: QueuedPacket) {
                self.queue.push_back(packet);
                self.packet_count += 1;
            }

            fn pop_packet(&mut self) -> Option<QueuedPacket> {
                let packet = self.queue.pop_front()?;

                let time_spent_queued = self
                    .last_update
                    .map(|last_update| last_update - packet.queued_at)
                    .unwrap_or(Duration::ZERO);
                self.total_time_spent_queued = self
                    .total_time_spent_queued
                    .saturating_sub(time_spent_queued);
                self.packet_count -= 1;

                Some(packet)
            }

            fn is_empty(&self) -> bool {
                self.queue.is_empty()
            }

            fn update_average_queue_time(&mut self, now: Instant) {
                let Some(last_update) = self.last_update else {
                    self.last_update = Some(now);
                    return;
                };

                let elapsed = now - last_update;
                self.total_time_spent_queued += elapsed * self.packet_count;
                self.last_update = Some(now);
            }

            fn queue_state(&self, now: Instant) -> QueueState {
                QueueState {
                    midrid: self.midrid,
                    unpaced: self.is_audio,
                    use_for_padding: !self.is_audio && self.last_emitted.is_some(),
                    snapshot: QueueSnapshot {
                        created_at: now,
                        byte_size: self.queue.iter().map(QueuedPacket::size).sum(),
                        packet_count: self.packet_count,
                        total_queue_time_origin: self.total_time_spent_queued,
                        last_emitted: self.last_emitted,
                        first_unsent: self.queue.iter().next().map(|p| p.queued_at),
                        priority: self.priority,
                    },
                }
            }
        }

        use std::fmt;

        impl fmt::Display for PacketKind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    PacketKind::Audio => write!(f, "audio"),
                    PacketKind::Video => write!(f, "video"),
                    PacketKind::Padding => write!(f, "padding"),
                }
            }
        }
    }
}
