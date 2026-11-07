//! Sans-IO network emulator inspired by Linux netem.
//!
//! This crate provides a network emulator that can simulate:
//! - Latency and jitter
//! - Packet loss (random or bursty via Gilbert-Elliot model)
//! - Packet duplication
//! - Packet reordering
//! - Rate limiting
//!
//! # Sans-IO Pattern
//!
//! This implementation follows the Sans-IO pattern: packets go in with timestamps,
//! and decisions come out (drop, delay, duplicate). The caller handles actual I/O
//! and timing.
//!
//! # Example
//!
//! ```
//! use std::time::{Duration, Instant};
//! use str0m_netem::{Netem, NetemConfig, Input, Output, LossModel, RandomLoss, Probability};
//!
//! let config = NetemConfig::new()
//!     .latency(Duration::from_millis(50))
//!     .jitter(Duration::from_millis(10))
//!     .loss(RandomLoss::new(Probability::new(0.01)))
//!     .seed(42);
//!
//! let mut netem: Netem<Vec<u8>> = Netem::new(config);
//!
//! // Send a packet
//! let now = Instant::now();
//! netem.handle_input(Input::Packet(now, vec![1, 2, 3]));
//!
//! // Poll for output
//! while let Some(output) = netem.poll_output() {
//!     match output {
//!         Output::Timeout(when) => {
//!             // Wait until `when` and call handle_input with Input::Timeout
//!         }
//!         Output::Packet(data) => {
//!             // Send the packet
//!         }
//!     }
//! }
//! ```

// Avoid rewriting existing code: nested conditions are often more readable than long if-chains.
#![allow(clippy::collapsible_if)]

mod config;
mod loss;

pub use config::{Bitrate, DataSize, GilbertElliot, Link};
pub use config::{LossModel, NetemConfig, Probability, RandomLoss};

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use fastrand::Rng;

use loss::LossState;

/// Sans-IO network emulator.
pub struct Netem<T> {
    config: NetemConfig,
    rng: Rng,
    loss_state: LossState,

    /// Priority queue of packets, ordered by send time (earliest first).
    /// Using Reverse to make BinaryHeap a min-heap.
    queue: BinaryHeap<Reverse<QueuedPacket<T>>>,

    /// Last delay value for correlation.
    last_delay: Duration,

    /// Virtual time when the last packet would finish transmitting (for rate limiting).
    rate_virtual_time: Option<Instant>,

    /// Counter for reordering (every N packets, one gets reordered).
    reorder_counter: u32,

    /// Current time from the last input.
    current_time: Option<Instant>,

    /// Number of packets for the current time.
    packet_count: u64,

    /// Whether we've already returned a timeout for the next packet.
    timeout_pending: bool,

    /// Send time of the last queued packet (for reordering).
    last_send_at: Option<Instant>,

    /// Total bytes currently queued (for buffer overflow detection).
    queued_bytes: usize,
}

impl<T: Clone + WithLen> Netem<T> {
    /// Create a new network emulator with the given configuration.
    pub fn new(config: NetemConfig) -> Self {
        let rng = Rng::with_seed(config.seed);

        let loss_state = LossState::new(&config.loss);

        Self {
            config,
            rng,
            loss_state,
            queue: BinaryHeap::new(),
            last_delay: Duration::ZERO,
            rate_virtual_time: None,
            reorder_counter: 0,
            current_time: None,
            packet_count: 0,
            timeout_pending: false,
            last_send_at: None,
            queued_bytes: 0,
        }
    }

    /// Handle an input event.
    pub fn handle_input(&mut self, input: Input<T>) {
        match input {
            Input::Timeout(now) => {
                self.progress_time(now);
                self.timeout_pending = false;
            }
            Input::Packet(now, data) => {
                self.progress_time(now);
                self.process_packet(now, data);
            }
        }
    }

    fn progress_time(&mut self, now: Instant) {
        if let Some(last_time) = self.current_time {
            if now < last_time {
                // Time does not go backwards.
                return;
            }
        }
        self.current_time = Some(now);
    }

    /// Poll for the next output event.
    ///
    /// Returns `None` when there are no more events to process.
    pub fn poll_output(&mut self) -> Option<Output<T>> {
        let now = self.current_time?;

        // Check if the next packet is ready to send
        if let Some(Reverse(packet)) = self.queue.peek() {
            if packet.send_at <= now {
                let Reverse(packet) = self.queue.pop().unwrap();
                // Decrement queued bytes when packet is dequeued
                self.queued_bytes = self.queued_bytes.saturating_sub(packet.data.len());
                return Some(Output::Packet(packet.data));
            }

            // Need to wait for the packet
            if !self.timeout_pending {
                self.timeout_pending = true;
                return Some(Output::Timeout(packet.send_at));
            }
        }

        None
    }

    /// Returns when the next packet will be ready, if any.
    ///
    /// This can be used to decide which of multiple Netem instances
    /// should be polled next.
    pub fn poll_timeout(&self) -> Instant {
        self.queue
            .peek()
            .map(|Reverse(p)| p.send_at)
            .unwrap_or_else(not_happening)
    }

    /// Process an incoming packet.
    fn process_packet(&mut self, now: Instant, data: T) {
        // Check for packet loss
        if self
            .loss_state
            .should_lose(&self.config.loss, &mut self.rng)
        {
            return; // Packet dropped
        }

        // Check for duplication (process original first, then maybe duplicate)
        let should_duplicate = self.rng.f32() < self.config.duplicate.0;

        // Process the original packet
        self.enqueue_packet(now, data.clone());

        // Duplicate if needed
        if should_duplicate {
            self.enqueue_packet(now, data);
        }
    }

    /// Calculate delay and enqueue a packet.
    fn enqueue_packet(&mut self, now: Instant, data: T) {
        // Calculate delay with jitter
        let delay = self.calculate_delay();

        // Calculate base send time
        let mut send_at = now + delay;

        // Handle link rate limiting and buffer overflow
        let transmission_time = if let Some(link) = self.config.link {
            let packet_size = DataSize::from(data.len());
            let tx_time = packet_size / link.rate;

            // Apply rate limiting: packet can't be sent until previous finishes
            if let Some(virtual_time) = self.rate_virtual_time {
                if virtual_time > send_at {
                    send_at = virtual_time;
                }
            }

            // Check buffer overflow (tail drop) using actual queued bytes
            if self.queued_bytes + data.len() > link.buffer.as_bytes_usize() {
                // Buffer overflow - drop packet
                return;
            }

            Some(tx_time)
        } else {
            None
        };

        // Determine if this packet should be reordered
        let should_reorder = if let Some(gap) = self.config.reorder_gap {
            self.reorder_counter += 1;
            if self.reorder_counter >= gap {
                self.reorder_counter = 0;
                // Can only reorder if we have a previous packet to reorder before
                self.last_send_at.is_some() && self.packet_count > 0
            } else {
                false
            }
        } else {
            false
        };

        let gap = self.config.reorder_gap.unwrap_or(1) as u64;
        let packet_index;

        if should_reorder {
            // Reordered packet: use previous packet's send_at and a lower index
            send_at = self.last_send_at.unwrap();
            // Index slots before the previous packet: count * gap - 1
            // Previous packet had index = count * gap
            packet_index = self.packet_count * gap - 1;
            // Don't update rate_virtual_time or last_send_at
        } else {
            // Normal packet: use calculated send_at with gaps for index
            packet_index = (self.packet_count + 1) * gap;

            // Update rate_virtual_time for next packet
            if let Some(tx_time) = transmission_time {
                self.rate_virtual_time = Some(send_at + tx_time);
            }

            // Track this packet's send_at for potential future reordering
            self.last_send_at = Some(send_at);
        }

        self.packet_count += 1;

        // Track queued bytes for buffer overflow detection
        self.queued_bytes += data.len();

        let packet = QueuedPacket {
            send_at,
            data,
            packet_index,
        };
        self.queue.push(Reverse(packet));

        // Reset timeout pending since queue changed
        self.timeout_pending = false;
    }

    /// Calculate delay with jitter and correlation.
    fn calculate_delay(&mut self) -> Duration {
        let base = self.config.latency;
        let jitter = self.config.jitter;

        if jitter.is_zero() {
            return base;
        }

        // Generate correlated jitter
        let rho = self.config.delay_correlation.0;
        let jitter_nanos = jitter.as_nanos() as f32;

        // Random value in [-1, 1]
        let fresh_random = self.rng.f32() * 2.0 - 1.0;
        let last_normalized = if self.last_delay >= base {
            (self.last_delay - base).as_nanos() as f32 / jitter_nanos
        } else {
            -((base - self.last_delay).as_nanos() as f32 / jitter_nanos)
        };

        let jitter_factor = if rho == 0.0 {
            fresh_random
        } else {
            fresh_random * (1.0 - rho) + last_normalized.clamp(-1.0, 1.0) * rho
        };

        let jitter_nanos = (jitter_factor * jitter_nanos) as i64;
        let delay = if jitter_nanos >= 0 {
            base + Duration::from_nanos(jitter_nanos as u64)
        } else {
            base.saturating_sub(Duration::from_nanos((-jitter_nanos) as u64))
        };

        self.last_delay = delay;
        delay
    }

    /// Returns the number of packets currently queued.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Returns true if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Update the configuration without dropping queued packets.
    ///
    /// Resets loss state and correlation tracking, but preserves timing state
    /// (rate limiting, reorder counter, pending timeouts) to avoid disrupting
    /// packets already in the queue.
    pub fn set_config(&mut self, config: NetemConfig) {
        self.rng = Rng::with_seed(config.seed);
        self.loss_state = LossState::new(&config.loss);
        self.last_delay = Duration::ZERO;
        self.last_send_at = None;
        // Don't reset: rate_virtual_time, reorder_counter, timeout_pending, current_time
        // These affect packets already queued
        self.config = config;
    }
}

/// Trait for getting the length of packet data (used for rate limiting).
pub trait WithLen {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: AsRef<[u8]>> WithLen for T {
    fn len(&self) -> usize {
        self.as_ref().len()
    }
}

/// Input events to the network emulator.
#[derive(Debug)]
pub enum Input<T> {
    /// A timeout has occurred at the given instant.
    Timeout(Instant),

    /// A packet arrived at the given instant with the given data.
    Packet(Instant, T),
}

/// Output events from the network emulator.
#[derive(Debug)]
pub enum Output<T> {
    /// Request a timeout at the given instant.
    Timeout(Instant),

    /// A packet is ready to be sent.
    Packet(T),
}

/// A queued packet waiting to be sent.
#[derive(Debug)]
struct QueuedPacket<T> {
    /// When this packet should be sent.
    send_at: Instant,

    /// The packet data.
    data: T,

    /// Ever increasing counter to break ties when send_at is the same.
    packet_index: u64,
}

fn not_happening() -> Instant {
    Instant::now() + Duration::from_secs(3600 * 24 * 365 * 10)
}

impl<T> PartialEq for QueuedPacket<T> {
    fn eq(&self, other: &Self) -> bool {
        self.send_at == other.send_at && self.packet_index == other.packet_index
    }
}

impl<T> Eq for QueuedPacket<T> {}

impl<T> PartialOrd for QueuedPacket<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for QueuedPacket<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.packet_index
            .cmp(&other.packet_index)
            .then(self.send_at.cmp(&other.send_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant() -> Instant {
        Instant::now()
    }

    #[test]
    fn test_passthrough() {
        let config = NetemConfig::default();
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();
        netem.handle_input(Input::Packet(now, vec![1, 2, 3]));

        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(data)) if data == vec![1, 2, 3]));
        assert!(netem.poll_output().is_none());
    }

    #[test]
    fn test_latency() {
        let config = NetemConfig::new()
            .latency(Duration::from_millis(100))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();
        netem.handle_input(Input::Packet(now, vec![1, 2, 3]));

        // Should get a timeout, not the packet
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Timeout(t)) if t > now));

        // After the timeout, packet should be ready
        let later = now + Duration::from_millis(100);
        netem.handle_input(Input::Timeout(later));

        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(data)) if data == vec![1, 2, 3]));
    }

    #[test]
    fn test_total_loss() {
        let config = NetemConfig::new()
            .loss(RandomLoss::new(Probability::ONE))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();
        netem.handle_input(Input::Packet(now, vec![1, 2, 3]));

        assert!(netem.poll_output().is_none());
        assert!(netem.is_empty());
    }

    #[test]
    fn test_duplication() {
        let config = NetemConfig::new().duplicate(Probability::ONE).seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();
        netem.handle_input(Input::Packet(now, vec![1, 2, 3]));

        // Should get two packets
        assert!(matches!(netem.poll_output(), Some(Output::Packet(_))));
        assert!(matches!(netem.poll_output(), Some(Output::Packet(_))));
        assert!(netem.poll_output().is_none());
    }

    #[test]
    fn test_rate_limiting() {
        // 8 kbps = 1000 bytes/sec, large buffer to avoid drops
        let config = NetemConfig::new()
            .link(Bitrate::kbps(8), DataSize::kbytes(10))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();

        // Send 100 bytes
        netem.handle_input(Input::Packet(now, vec![0; 100]));

        // First packet should be immediate
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(_))));

        // Send another 100 bytes immediately after
        netem.handle_input(Input::Packet(now, vec![0; 100]));

        // Second packet should require a timeout (rate limited)
        let output = netem.poll_output();
        match output {
            Some(Output::Timeout(t)) => {
                // Should be delayed by ~100ms (100 bytes at 1000 bytes/sec)
                let delay = t - now;
                assert!(delay >= Duration::from_millis(90));
                assert!(delay <= Duration::from_millis(110));
            }
            _ => panic!("Expected timeout, got {:?}", output),
        }
    }

    #[test]
    fn test_reordering() {
        let config = NetemConfig::new()
            .latency(Duration::from_millis(100))
            .reorder_gap(3) // Every 3rd packet is reordered
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();

        // Send 3 packets
        netem.handle_input(Input::Packet(now, vec![1]));
        netem.handle_input(Input::Packet(now, vec![2]));
        netem.handle_input(Input::Packet(now, vec![3])); // This one should be reordered before packet 2

        // All packets are delayed by latency, so we get a timeout first
        let output = netem.poll_output();
        assert!(
            matches!(output, Some(Output::Timeout(t)) if t == now + Duration::from_millis(100))
        );

        // After the timeout, packets should be ready in reordered sequence: 1, 3, 2
        let later = now + Duration::from_millis(100);
        netem.handle_input(Input::Timeout(later));

        // Packet 1 comes first (lowest index)
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(data)) if data == vec![1]));

        // Packet 3 comes second (reordered before packet 2)
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(data)) if data == vec![3]));

        // Packet 2 comes last
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(data)) if data == vec![2]));
    }

    #[test]
    fn test_reordering_with_rate_limiting() {
        // 8 kbps = 1024 bytes/sec, large buffer to avoid drops
        let config = NetemConfig::new()
            .link(Bitrate::kbps(8), DataSize::kbytes(10))
            .reorder_gap(3) // Every 3rd packet is reordered
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();

        // Send 3 packets of 100 bytes each
        netem.handle_input(Input::Packet(now, vec![0; 100]));
        netem.handle_input(Input::Packet(now, vec![0; 100]));
        netem.handle_input(Input::Packet(now, vec![0; 100])); // Reordered

        // First packet should be immediate (no latency configured)
        let output = netem.poll_output();
        assert!(matches!(output, Some(Output::Packet(_))));

        // Next output should be a timeout (rate limited)
        // The reordered packet (3rd) shares the slot with packet 2
        let output = netem.poll_output();
        match output {
            Some(Output::Timeout(t)) => {
                // Should be delayed by ~100ms (100 bytes at 1000 bytes/sec)
                let delay = t - now;
                assert!(
                    delay >= Duration::from_millis(90),
                    "Reordered packet should respect rate limiting, got delay {:?}",
                    delay
                );
            }
            _ => panic!(
                "Expected timeout for rate-limited reordered packet, got {:?}",
                output
            ),
        }
    }

    #[test]
    fn test_gilbert_elliot_preset() {
        let config = NetemConfig::new()
            .loss(LossModel::GilbertElliot(GilbertElliot::wifi()))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();
        let mut received = 0;
        let total = 1000;

        for i in 0..total {
            netem.handle_input(Input::Packet(now, vec![i as u8]));
            while let Some(output) = netem.poll_output() {
                if matches!(output, Output::Packet(_)) {
                    received += 1;
                }
            }
        }

        // WiFi preset should have ~1% loss, so ~990 received
        let loss_ratio = 1.0 - (received as f32 / total as f32);
        assert!(
            (0.005..=0.05).contains(&loss_ratio),
            "Loss ratio: {}",
            loss_ratio
        );
    }

    #[test]
    fn test_buffer_overflow_drops_packets() {
        // 80 kbps = 10KB/sec, tiny 100 byte buffer
        // This means only ~1 packet of 100 bytes can be queued
        let config = NetemConfig::new()
            .link(Bitrate::kbps(80), DataSize::bytes(100))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();

        // Send 5 packets of 100 bytes each at once
        // Only the first should be accepted, rest should be dropped due to buffer overflow
        for i in 0..5 {
            netem.handle_input(Input::Packet(now, vec![i; 100]));
        }

        // Count how many packets we actually receive
        let mut received = 0;
        while let Some(output) = netem.poll_output() {
            match output {
                Output::Packet(_) => received += 1,
                Output::Timeout(t) => {
                    // Advance time to release next packet
                    netem.handle_input(Input::Timeout(t));
                }
            }
        }

        // With a 100 byte buffer and 100 byte packets, only 1-2 should fit
        assert!(
            received < 5,
            "Expected buffer overflow to drop packets, but received all {received}"
        );
        assert!(
            received >= 1,
            "Expected at least one packet to be delivered, got {received}"
        );
    }

    #[test]
    fn test_congestion_causes_delay_then_loss() {
        // 80 kbps = 10KB/sec, 500 byte buffer (~50ms worth at this rate)
        // This allows ~5 packets of 100 bytes each to be queued
        let config = NetemConfig::new()
            .link(Bitrate::kbps(80), DataSize::bytes(500))
            .seed(42);
        let mut netem: Netem<Vec<u8>> = Netem::new(config);

        let now = instant();

        // Send many packets to cause congestion and buffer overflow
        // 20 packets * 100 bytes = 2000 bytes, but buffer is only 500 bytes
        for i in 0..20 {
            netem.handle_input(Input::Packet(now, vec![i; 100]));
        }

        // First packet should be immediate (no queue yet)
        let first = netem.poll_output();
        assert!(matches!(first, Some(Output::Packet(_))));

        // Next should be a timeout (queued due to rate limiting)
        let second = netem.poll_output();
        match second {
            Some(Output::Timeout(t)) => {
                // Delay should be positive (congestion causing queue buildup)
                assert!(t > now, "Expected queuing delay");
            }
            _ => panic!("Expected timeout due to rate limiting"),
        }

        // Advance time to release all remaining packets
        // Send a dummy packet at a far future time to advance current_time
        let far_future = now + Duration::from_secs(10);
        netem.handle_input(Input::Packet(far_future, vec![]));

        // Count total packets received (including the first one we already got)
        let mut received = 1;
        while let Some(output) = netem.poll_output() {
            if matches!(output, Output::Packet(_)) {
                received += 1;
            }
        }

        // Should have lost some packets due to buffer overflow
        // With 500 byte buffer and 100 byte packets:
        // - First packet sent immediately (no queue)
        // - 5 more packets can be buffered (500 bytes)
        // - Remaining 14 packets should be dropped
        // Expected: ~6 packets total (plus the dummy 0-byte packet)
        assert!(
            received < 20,
            "Expected buffer overflow to cause loss, but received all {received}"
        );
        assert!(
            received >= 5,
            "Expected some packets to get through, only got {received}"
        );
    }
}
