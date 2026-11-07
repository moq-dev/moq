use std::time::Duration;

pub use str0m_proto::{Bitrate, DataSize};

/// Configuration for the network emulator.
///
/// Use the builder pattern to configure the emulator:
///
/// ```
/// use std::time::Duration;
/// use str0m_netem::{NetemConfig, LossModel, GilbertElliot};
///
/// let config = NetemConfig::new()
///     .latency(Duration::from_millis(50))
///     .jitter(Duration::from_millis(10))
///     .loss(GilbertElliot::wifi())
///     .seed(42);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct NetemConfig {
    pub(crate) latency: Duration,
    pub(crate) jitter: Duration,
    pub(crate) delay_correlation: Probability,
    pub(crate) loss: LossModel,
    pub(crate) duplicate: Probability,
    pub(crate) reorder_gap: Option<u32>,
    pub(crate) link: Option<Link>,
    pub(crate) seed: u64,
}

/// A bottleneck link with rate limiting and finite buffer.
///
/// When packets arrive faster than the link rate, they queue up.
/// When the queue exceeds the buffer size, packets are dropped (tail drop).
#[derive(Debug, Clone, Copy)]
pub struct Link {
    /// Link capacity in bits per second.
    pub(crate) rate: Bitrate,
    /// Buffer size in bytes. When exceeded, packets are dropped.
    pub(crate) buffer: DataSize,
}

impl Default for NetemConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl NetemConfig {
    /// Create a new network emulator configuration with default values.
    ///
    /// Default: no delay, no loss, no rate limit, seed 0.
    pub const fn new() -> Self {
        Self {
            latency: Duration::ZERO,
            jitter: Duration::ZERO,
            delay_correlation: Probability::ZERO,
            loss: LossModel::None,
            duplicate: Probability::ZERO,
            reorder_gap: None,
            link: None,
            seed: 0,
        }
    }

    // --- Preset constructors for common network environments ---

    /// Good WiFi: low latency, minimal loss, high bandwidth.
    ///
    /// Simulates a typical home/office WiFi connection with good signal.
    /// - Latency: 5ms (local network)
    /// - Jitter: 2ms
    /// - Loss: ~1% bursty (GilbertElliot::wifi)
    /// - Bandwidth: 100 Mbps with 200 KB buffer
    pub fn wifi() -> Self {
        Self::new()
            .latency(Duration::from_millis(5))
            .jitter(Duration::from_millis(2))
            .loss(GilbertElliot::wifi())
            .link(Bitrate::mbps(100), DataSize::kbytes(200))
    }

    /// Lossy WiFi: poor signal, more interference and loss.
    ///
    /// Simulates WiFi with weak signal or interference.
    /// - Latency: 15ms
    /// - Jitter: 10ms
    /// - Loss: ~5% bursty (GilbertElliot::wifi_lossy)
    /// - Bandwidth: 50 Mbps with 100 KB buffer
    pub fn wifi_lossy() -> Self {
        Self::new()
            .latency(Duration::from_millis(15))
            .jitter(Duration::from_millis(10))
            .loss(GilbertElliot::wifi_lossy())
            .link(Bitrate::mbps(50), DataSize::kbytes(100))
    }

    /// Congested WiFi: shared connection with competing traffic.
    ///
    /// Simulates a home WiFi where others are streaming video,
    /// causing buffer bloat and bandwidth contention.
    /// - Latency: 10ms
    /// - Jitter: 20ms (high due to competing traffic)
    /// - Loss: ~10% (buffer overflow from congestion)
    /// - Bandwidth: 5 Mbps with 30 KB buffer (your share of the link)
    pub fn wifi_congested() -> Self {
        Self::new()
            .latency(Duration::from_millis(10))
            .jitter(Duration::from_millis(20))
            .loss(GilbertElliot::congested())
            .link(Bitrate::mbps(5), DataSize::kbytes(30))
    }

    /// Cellular/4G: moderate latency with occasional handoff loss.
    ///
    /// Simulates a mobile LTE/4G connection.
    /// - Latency: 50ms
    /// - Jitter: 15ms
    /// - Loss: ~2% bursty (handoffs, signal fading)
    /// - Bandwidth: 30 Mbps with 100 KB buffer
    pub fn cellular() -> Self {
        Self::new()
            .latency(Duration::from_millis(50))
            .jitter(Duration::from_millis(15))
            .loss(GilbertElliot::cellular())
            .link(Bitrate::mbps(30), DataSize::kbytes(100))
    }

    /// Satellite (GEO): very high latency, weather-related loss bursts.
    ///
    /// Simulates a geostationary satellite link (e.g., Viasat, HughesNet).
    /// - Latency: 600ms (round-trip to GEO orbit)
    /// - Jitter: 30ms
    /// - Loss: ~3% bursty (weather, atmospheric)
    /// - Bandwidth: 15 Mbps with 500 KB buffer (large buffer for high BDP)
    pub fn satellite() -> Self {
        Self::new()
            .latency(Duration::from_millis(600))
            .jitter(Duration::from_millis(30))
            .loss(GilbertElliot::satellite())
            .link(Bitrate::mbps(15), DataSize::kbytes(500))
    }

    /// Congested network: high latency/jitter, frequent loss, throttled.
    ///
    /// Simulates a severely congested network path.
    /// - Latency: 80ms
    /// - Jitter: 40ms
    /// - Loss: ~10% (queue overflow)
    /// - Bandwidth: 10 Mbps with 50 KB buffer (small buffer causes loss)
    pub fn congested() -> Self {
        Self::new()
            .latency(Duration::from_millis(80))
            .jitter(Duration::from_millis(40))
            .loss(GilbertElliot::congested())
            .link(Bitrate::mbps(10), DataSize::kbytes(50))
    }

    /// Set fixed delay added to every packet.
    ///
    /// This simulates the propagation delay of a network link. For example:
    /// - LAN: 0-1ms
    /// - Same city: 5-20ms
    /// - Cross-country: 30-70ms
    /// - Intercontinental: 100-200ms
    /// - Satellite (GEO): 500-700ms
    ///
    /// The actual send time is: `arrival_time + latency + random_jitter`
    pub fn latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Set random variation added to the latency (uniform distribution).
    ///
    /// Each packet gets a random delay in the range `[-jitter, +jitter]` added
    /// to its base latency. This simulates the variable queuing delays in routers.
    ///
    /// For example, with `latency(50ms)` and `jitter(10ms)`, packets will have
    /// delays uniformly distributed between 40ms and 60ms.
    ///
    /// Real networks typically have jitter of 1-30ms depending on congestion.
    pub fn jitter(mut self, jitter: Duration) -> Self {
        self.jitter = jitter;
        self
    }

    /// Set correlation between consecutive delay values.
    ///
    /// Controls how similar each packet's delay is to the previous packet's delay.
    /// This models the fact that network conditions change gradually, not randomly.
    ///
    /// - `0.0`: Each packet's jitter is completely independent (unrealistic)
    /// - `0.25`: Low correlation, delays vary somewhat smoothly
    /// - `0.5`: Medium correlation, delays change gradually
    /// - `0.75`: High correlation, delays are very similar to previous
    /// - `1.0`: Perfect correlation, all packets get the same jitter (no variation)
    ///
    /// Formula: `next_jitter = random * (1 - correlation) + last_jitter * correlation`
    ///
    /// A value around 0.25-0.5 is realistic for most networks.
    pub fn delay_correlation(mut self, correlation: Probability) -> Self {
        self.delay_correlation = correlation;
        self
    }

    /// Set the loss model.
    ///
    /// See [`LossModel`] for available options.
    pub fn loss(mut self, loss: impl Into<LossModel>) -> Self {
        self.loss = loss.into();
        self
    }

    /// Set probability that each packet is duplicated.
    ///
    /// When a packet is duplicated, both the original and the copy are sent
    /// with the same delay. This simulates network equipment bugs or
    /// misconfigured routing that causes packets to be sent twice.
    ///
    /// - `0.0`: No duplication (normal)
    /// - `0.01`: 1% of packets are duplicated (rare but happens)
    /// - `0.1`: 10% duplication (severe misconfiguration)
    pub fn duplicate(mut self, probability: Probability) -> Self {
        self.duplicate = probability;
        self
    }

    /// Set reordering by sending every Nth packet immediately.
    ///
    /// Every Nth packet bypasses the delay queue and is sent immediately,
    /// causing it to arrive before packets that were sent earlier.
    /// This simulates multi-path routing where packets take different routes.
    ///
    /// - `3`: Every 3rd packet is sent immediately
    /// - `10`: Every 10th packet is sent immediately
    ///
    /// Combined with latency, this creates realistic reordering patterns.
    pub fn reorder_gap(mut self, gap: u32) -> Self {
        self.reorder_gap = Some(gap);
        self
    }

    /// Set a bottleneck link with rate limiting and finite buffer.
    ///
    /// Simulates a network link with limited capacity. When packets arrive faster
    /// than the link can transmit, they queue up. When the queue (buffer) is full,
    /// packets are dropped (tail drop), simulating congestion-induced loss.
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use str0m_netem::{NetemConfig, Bitrate, DataSize};
    ///
    /// // 10 Mbps link with 200KB buffer (~160ms at full rate)
    /// let config = NetemConfig::new()
    ///     .latency(Duration::from_millis(50))
    ///     .link(Bitrate::mbps(10), DataSize::bytes(200_000));
    /// ```
    ///
    /// # Behavior
    ///
    /// - Below capacity: packets flow with minimal queuing delay
    /// - At capacity: queuing delay increases as buffer fills
    /// - Over capacity: buffer fills, then excess packets are dropped
    pub fn link(mut self, rate: Bitrate, buffer: DataSize) -> Self {
        self.link = Some(Link { rate, buffer });
        self
    }

    /// Set seed for the random number generator.
    ///
    /// Using the same seed produces identical packet loss/delay patterns,
    /// which is essential for reproducible tests. Different seeds produce
    /// different (but deterministic) random sequences.
    ///
    /// - Use a fixed seed (e.g., `42`) for reproducible tests
    /// - Use `std::time::SystemTime::now().duration_since(UNIX_EPOCH).as_nanos() as u64`
    ///   for random behavior in production
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

/// Probability in range 0.0..=1.0
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Probability(pub f32);

impl Probability {
    pub const ZERO: Probability = Probability(0.0);
    pub const ONE: Probability = Probability(1.0);

    pub fn new(value: f32) -> Self {
        debug_assert!(
            (0.0..=1.0).contains(&value),
            "Probability must be in 0.0..=1.0"
        );
        Probability(value.clamp(0.0, 1.0))
    }

    pub fn value(self) -> f32 {
        self.0
    }
}

/// Loss model for packet dropping.
///
/// Real networks rarely have uniform random loss. Instead, losses tend to come
/// in bursts due to congestion, interference, or route changes. This enum
/// provides different models to simulate various loss patterns.
#[derive(Debug, Clone, Copy, Default)]
pub enum LossModel {
    /// No packet loss. All packets are delivered.
    #[default]
    None,

    /// Probability-based random loss with optional correlation.
    ///
    /// See [`RandomLoss`] for configuration options.
    Random(RandomLoss),

    /// Gilbert-Elliot model for realistic bursty packet loss.
    ///
    /// This 2-state Markov model alternates between GOOD (low loss) and BAD
    /// (high loss) states, producing realistic burst patterns where losses
    /// cluster together rather than being spread evenly.
    ///
    /// Use the builder methods or presets on [`GilbertElliot`]:
    /// - `GilbertElliot::wifi()` - occasional short bursts
    /// - `GilbertElliot::cellular()` - moderate bursts from handoffs
    /// - `GilbertElliot::satellite()` - rare but longer bursts
    /// - `GilbertElliot::congested()` - frequent drops
    GilbertElliot(GilbertElliot),
}

/// Random loss model configuration.
///
/// Each packet has the given probability of being dropped. By default, each
/// decision is independent (Bernoulli process), producing unrealistic "spread out"
/// loss patterns.
///
/// Use [`correlation`](RandomLoss::correlation) to make losses more bursty, or
/// prefer [`GilbertElliot`] for more realistic bursty loss with finer control.
///
/// # Example
///
/// ```
/// use str0m_netem::{RandomLoss, Probability};
///
/// // 5% loss, independent
/// let simple = RandomLoss::new(Probability::new(0.05));
///
/// // 5% loss, bursty (losses cluster together)
/// let bursty = RandomLoss::new(Probability::new(0.05))
///     .correlation(Probability::new(0.5));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RandomLoss {
    /// Probability of dropping each packet.
    pub(crate) probability: f32,

    /// Correlation between consecutive loss decisions.
    pub(crate) correlation: f32,
}

impl RandomLoss {
    /// Create a new random loss model with the given drop probability.
    ///
    /// Loss decisions are independent by default (no correlation).
    pub fn new(probability: Probability) -> Self {
        Self {
            probability: probability.0,
            correlation: 0.0,
        }
    }

    /// Set correlation between consecutive loss decisions.
    ///
    /// Controls how "bursty" random loss is:
    ///
    /// - `0.0`: Each loss decision is independent (Bernoulli process)
    /// - `0.5`: If a packet was lost, next packet is more likely to be lost too
    /// - `0.9`: Very bursty - losses come in clusters
    ///
    /// For realistic bursty loss, consider using [`GilbertElliot`] instead,
    /// which provides more control over burst characteristics.
    pub fn correlation(mut self, correlation: Probability) -> Self {
        self.correlation = correlation.0;
        self
    }
}

/// Gilbert-Elliot loss model with two states: GOOD and BAD.
///
/// This is a 2-state Markov chain that models bursty packet loss:
///
/// ```text
///              p (enter burst)
///         ┌──────────────────────┐
///         │                      ▼
///     ┌───────┐              ┌───────┐
///     │ GOOD  │              │  BAD  │
///     │(k=0%) │              │(h=100%)│
///     └───────┘              └───────┘
///         ▲                      │
///         └──────────────────────┘
///              r (exit burst)
/// ```
///
/// **How it works:**
/// 1. Start in GOOD state (low/no loss)
/// 2. Each packet, roll dice to potentially transition to BAD state (probability `p`)
/// 3. In BAD state, packets are lost with probability `h` (default 100%)
/// 4. Each packet in BAD, roll dice to return to GOOD (probability `r`)
///
/// **Key insight:** The average number of packets before transitioning is `1/probability`.
/// So `p = 0.01` means ~100 packets in GOOD before entering BAD,
/// and `r = 0.5` means ~2 packets in BAD before returning to GOOD.
///
/// # Example
///
/// ```
/// use str0m_netem::GilbertElliot;
///
/// // Custom: lose ~3 packets every ~50 packets
/// let ge = GilbertElliot::new()
///     .good_duration(50.0)   // stay in GOOD for ~50 packets
///     .bad_duration(3.0);    // stay in BAD for ~3 packets (burst length)
///
/// // Or use a preset
/// let wifi = GilbertElliot::wifi();
/// ```
#[derive(Debug, Clone, Copy)]
pub struct GilbertElliot {
    /// Probability of transitioning from GOOD to BAD state (per packet).
    /// Average packets in GOOD = 1/p
    pub(crate) p: f32,

    /// Probability of transitioning from BAD to GOOD state (per packet).
    /// Average packets in BAD (burst length) = 1/r
    pub(crate) r: f32,

    /// Probability of loss when in BAD state (default 1.0 = 100%).
    pub(crate) h: f32,

    /// Probability of loss when in GOOD state (default 0.0 = 0%).
    pub(crate) k: f32,
}

impl Default for GilbertElliot {
    fn default() -> Self {
        Self::new()
    }
}

impl GilbertElliot {
    /// Create a new Gilbert-Elliot model with default parameters.
    ///
    /// Defaults produce **no loss**: stays in GOOD forever (p=0).
    /// Use the builder methods to configure loss behavior.
    pub fn new() -> Self {
        Self {
            p: 0.0, // never transition to BAD
            r: 1.0, // immediately return to GOOD
            h: 1.0, // 100% loss in BAD
            k: 0.0, // 0% loss in GOOD
        }
    }

    /// Set average number of packets in GOOD state before transitioning to BAD.
    ///
    /// This controls how often bursts occur. Higher values = rarer bursts.
    ///
    /// - `50.0`: Burst starts every ~50 packets on average
    /// - `100.0`: Burst starts every ~100 packets on average
    /// - `200.0`: Burst starts every ~200 packets on average
    ///
    /// Internally sets `p = 1 / avg_packets`.
    pub fn good_duration(mut self, avg_packets: f32) -> Self {
        self.p = if avg_packets > 0.0 {
            1.0 / avg_packets
        } else {
            0.0
        };
        self
    }

    /// Set average number of packets in BAD state (burst length).
    ///
    /// This controls how long each burst lasts. Higher values = longer bursts.
    ///
    /// - `1.0`: Single packet losses (not really bursty)
    /// - `2.0`: ~2 packets lost per burst
    /// - `5.0`: ~5 packets lost per burst
    /// - `10.0`: ~10 packets lost per burst (severe)
    ///
    /// Internally sets `r = 1 / avg_packets`.
    pub fn bad_duration(mut self, avg_packets: f32) -> Self {
        self.r = if avg_packets > 0.0 {
            1.0 / avg_packets
        } else {
            1.0
        };
        self
    }

    /// Set loss probability when in BAD state.
    ///
    /// Default is `1.0` (100% loss in BAD state).
    ///
    /// Setting this below 1.0 means some packets survive even during a burst,
    /// which can model partial outages or congestion that drops some but not
    /// all packets.
    pub fn loss_in_bad(mut self, prob: Probability) -> Self {
        self.h = prob.0;
        self
    }

    /// Set loss probability when in GOOD state.
    ///
    /// Default is `0.0` (no loss in GOOD state).
    ///
    /// Setting this above 0.0 adds a baseline random loss even outside of
    /// bursts, simulating a network that always has some background loss.
    pub fn loss_in_good(mut self, prob: Probability) -> Self {
        self.k = prob.0;
        self
    }

    // --- Preset constructors for common environments ---

    /// Good WiFi: rare short bursts (~1% loss).
    ///
    /// Models occasional interference causing brief packet loss.
    pub fn wifi() -> Self {
        Self::new()
            .good_duration(200.0) // ~200 packets between bursts
            .bad_duration(2.0) // ~2 packets lost per burst
            .loss_in_bad(Probability::ONE)
    }

    /// Lossy WiFi: frequent short bursts (~5% loss).
    ///
    /// Models poor WiFi signal with frequent interference.
    pub fn wifi_lossy() -> Self {
        Self::new()
            .good_duration(40.0) // ~40 packets between bursts
            .bad_duration(2.0) // ~2 packets lost per burst
            .loss_in_bad(Probability::ONE)
    }

    /// Mobile/cellular: moderate bursts from handoffs (~2% loss).
    ///
    /// Models cellular network with occasional handoffs and signal issues.
    pub fn cellular() -> Self {
        Self::new()
            .good_duration(100.0) // ~100 packets between bursts
            .bad_duration(2.0) // ~2 packets lost per burst
            .loss_in_bad(Probability::ONE)
    }

    /// Satellite: rare but longer bursts (~3% loss).
    ///
    /// Models satellite link with weather-related outages.
    pub fn satellite() -> Self {
        Self::new()
            .good_duration(100.0) // ~100 packets between bursts
            .bad_duration(3.0) // ~3 packets lost per burst
            .loss_in_bad(Probability::ONE)
    }

    /// Congested network: frequent drops (~10% loss).
    ///
    /// Models a heavily loaded network with queue overflow.
    pub fn congested() -> Self {
        Self::new()
            .good_duration(20.0) // ~20 packets between bursts
            .bad_duration(2.0) // ~2 packets lost per burst
            .loss_in_bad(Probability::ONE)
    }
}

impl From<RandomLoss> for LossModel {
    fn from(value: RandomLoss) -> Self {
        LossModel::Random(value)
    }
}

impl From<GilbertElliot> for LossModel {
    fn from(value: GilbertElliot) -> Self {
        LossModel::GilbertElliot(value)
    }
}
