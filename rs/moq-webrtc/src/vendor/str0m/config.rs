use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Rtc;
use crate::config::DtlsCert;
use crate::crypto::CryptoProvider;
use crate::crypto::dtls::DtlsVersion;
use crate::format::CodecConfig;
use crate::format::Vp9PacketizerMode;
use crate::ice::IceCreds;
use crate::io::DATAGRAM_MTU_TARGET;
use crate::io::DATAGRAM_MTU_TARGET_MAX;
use crate::io::DATAGRAM_MTU_TARGET_MIN;
use crate::io::DATAGRAM_MTU_WARN;
use crate::rtp_::{Bitrate, Extension, ExtensionMap};

/// Customized config for creating an [`Rtc`] instance.
///
/// ```
/// # #[cfg(feature = "openssl")] {
/// use std::time::Instant;
/// use str0m::RtcConfig;
///
/// let rtc = RtcConfig::new()
///     .set_ice_lite(true)
///     .build(Instant::now());
/// # }
/// ```
///
/// Configs implement [`Clone`] to help create multiple `Rtc` instances.
#[derive(Debug, Clone)]
pub struct RtcConfig {
    pub(crate) local_ice_credentials: Option<IceCreds>,
    pub(crate) crypto_provider: Option<Arc<CryptoProvider>>,
    pub(crate) dtls_cert: Option<DtlsCert>,
    pub(crate) fingerprint_verification: bool,
    pub(crate) ice_lite: bool,
    pub(crate) initial_stun_rto: Option<Duration>,
    pub(crate) max_stun_rto: Option<Duration>,
    pub(crate) max_stun_retransmits: Option<usize>,
    pub(crate) codec_config: CodecConfig,
    pub(crate) exts: ExtensionMap,
    pub(crate) stats_interval: Option<Duration>,
    pub(crate) intervals: RtcpReportIntervals,
    pub(crate) bwe_config: Option<BweConfig>,
    pub(crate) reordering_size_audio: usize,
    pub(crate) reordering_size_video: usize,
    pub(crate) reordering_timeout_audio: Option<Duration>,
    pub(crate) pause_threshold: Duration,
    pub(crate) send_buffer_audio: usize,
    pub(crate) send_buffer_video: usize,
    pub(crate) sctp_max_buffered_amount: usize,
    pub(crate) sctp_max_message_size: u32,
    pub(crate) rtp_mode: bool,
    pub(crate) enable_raw_packets: bool,
    pub(crate) dtls_version: DtlsVersion,
    pub(crate) vp9_packetizer_mode: Vp9PacketizerMode,
    pub(crate) snap_enabled: bool,
    pub(crate) mtu: RangeInclusive<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct BweConfig {
    pub(crate) initial_bitrate: Bitrate,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RtcpReportIntervals {
    pub(crate) audio: Duration,
    pub(crate) video: Duration,
}

impl RtcpReportIntervals {
    pub(crate) fn for_audio(self, audio: bool) -> Duration {
        if audio { self.audio } else { self.video }
    }
}

impl RtcConfig {
    /// Creates a new default config.
    pub fn new() -> Self {
        RtcConfig::default()
    }

    /// Get the local ICE credentials, if set.
    ///
    /// If not specified, local credentials will be randomly generated when
    /// building the [`Rtc`] instance.
    pub fn local_ice_credentials(&self) -> &Option<IceCreds> {
        &self.local_ice_credentials
    }

    /// Explicitly sets local ICE credentials.
    pub fn set_local_ice_credentials(mut self, local_ice_credentials: IceCreds) -> Self {
        self.local_ice_credentials = Some(local_ice_credentials);
        self
    }

    /// Set the crypto provider.
    ///
    /// This overrides what is set in [`crate::crypto::CryptoProvider::install_process_default()`].
    pub fn set_crypto_provider(mut self, p: Arc<CryptoProvider>) -> Self {
        self.crypto_provider = Some(p);
        self
    }

    /// The configured crypto provider, if explicitly set.
    ///
    /// Returns `None` if not explicitly set via [`Self::set_crypto_provider()`].
    /// When `None`, the process default will be checked when building the [`Rtc`] instance.
    pub fn crypto_provider(&self) -> Option<&Arc<CryptoProvider>> {
        self.crypto_provider.as_ref()
    }

    /// Returns the configured DTLS certificate, if set.
    ///
    /// If not set, a certificate will be generated automatically.
    pub fn dtls_cert(&self) -> Option<&DtlsCert> {
        self.dtls_cert.as_ref()
    }

    /// Set a pregenerated DTLS certificate.
    ///
    /// If not set, a certificate will be generated automatically using
    /// the configured crypto provider.
    ///
    /// ```
    /// # use str0m::RtcConfig;
    /// # use str0m::crypto;
    ///
    /// let provider = crypto::from_feature_flags();
    /// let cert = provider.dtls_provider.generate_certificate().unwrap();
    /// let rtc_config = RtcConfig::default()
    ///     .set_dtls_cert(cert);
    /// ```
    pub fn set_dtls_cert(mut self, cert: DtlsCert) -> Self {
        self.dtls_cert = Some(cert);
        self
    }

    /// Toggle ice lite. Ice lite is a mode for WebRTC servers with public IP address.
    /// An [`Rtc`] instance in ice lite mode will not make STUN binding requests, but only
    /// answer to requests from the remote peer.
    ///
    /// See [ICE RFC][1]
    ///
    /// [1]: https://www.rfc-editor.org/rfc/rfc8445#page-13
    pub fn set_ice_lite(mut self, enabled: bool) -> Self {
        self.ice_lite = enabled;
        self
    }

    /// Sets the initial STUN retransmission timeout (RTO).
    ///
    /// This is the initial wait time before a STUN request is retransmitted.
    /// The timeout will double with each retry, starting from this value.
    ///
    /// Defaults to 250ms.
    pub fn set_initial_stun_rto(&mut self, rto: Duration) {
        self.initial_stun_rto = Some(rto);
    }

    /// Sets the maximum STUN retransmission timeout for the ICE agent.
    ///
    /// This is the upper bound for how long to wait between retransmissions.
    /// It also controls how often successful bindings are checked.
    ///
    /// Defaults to 3000ms.
    pub fn set_max_stun_rto(&mut self, rto: Duration) {
        self.max_stun_rto = Some(rto);
    }

    /// Sets the maximum number of retransmits for STUN messages.
    ///
    /// Defaults to 9.
    pub fn set_max_stun_retransmits(&mut self, num: usize) {
        self.max_stun_retransmits = Some(num);
    }

    /// Get fingerprint verification mode.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to true.
    /// assert!(config.fingerprint_verification());
    /// ```
    pub fn fingerprint_verification(&self) -> bool {
        self.fingerprint_verification
    }

    /// Toggle certificate fingerprint verification.
    ///
    /// By default the certificate fingerprint is verified.
    pub fn set_fingerprint_verification(mut self, enabled: bool) -> Self {
        self.fingerprint_verification = enabled;
        self
    }

    /// Tells whether ice lite is enabled.
    ///
    /// ```
    /// # #[cfg(feature = "openssl")] {
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to false.
    /// assert_eq!(config.ice_lite(), false);
    /// # }
    /// ```
    pub fn ice_lite(&self) -> bool {
        self.ice_lite
    }

    /// Lower level access to precise configuration of codecs (payload types).
    pub fn codec_config(&mut self) -> &mut CodecConfig {
        &mut self.codec_config
    }

    /// Clear all configured codecs.
    ///
    /// ```
    /// # #[cfg(feature = "openssl")] {
    /// # use std::time::Instant;
    /// # use str0m::RtcConfig;
    /// // For the session to use only OPUS and VP8.
    /// let mut rtc = RtcConfig::default()
    ///     .clear_codecs()
    ///     .enable_opus(true, false)
    ///     .enable_vp8(true)
    ///     .build(Instant::now());
    /// # }
    /// ```
    pub fn clear_codecs(mut self) -> Self {
        self.codec_config.clear();
        self
    }

    /// Enable opus audio codec, optionally with RFC 2198 RED (redundant audio).
    ///
    /// Enabled by default (without RED). `use_red` folds a RED payload type onto Opus so the
    /// receiver can recover packet loss without retransmission, at the cost of extra audio payload
    /// size. Each audio codec carries its own RED payload type. The redundancy depth is set with
    /// [`Self::set_red_distances`] (default one level, the previous frame), and wrapping can be
    /// toggled at runtime on the send side without renegotiation via
    /// [`DirectApi::set_red_send`][crate::change::DirectApi::set_red_send].
    pub fn enable_opus(mut self, enabled: bool, use_red: bool) -> Self {
        self.codec_config.enable_opus(enabled, use_red);
        self
    }

    /// Set the RFC 2198 RED send-side redundancy pattern: how many packets back each redundant
    /// level carries. `[1]` (the default) sends one level, the previous frame, as browsers do;
    /// `[1, 3, 5]` sends three levels for higher loss at the cost of more bandwidth.
    ///
    /// The pattern is sanitised (zeros and values deeper than the recovery depth are dropped, then
    /// sorted and de-duped) and always includes distance 1 as the recovery anchor (so `[2, 4]`
    /// becomes `[1, 2, 4]`), falling back to `[1]` if empty. This only sets the pattern; enable RED
    /// via the `use_red` argument of an audio codec (e.g. [`Self::enable_opus`]) for it to take effect.
    pub fn set_red_distances(mut self, distances: &[u32]) -> Self {
        self.codec_config.set_red_distances(distances);
        self
    }

    /// Enable PCM μ-law audio codec, optionally with RFC 2198 RED (see [`Self::enable_opus`] for
    /// `use_red`).
    ///
    /// This is 14-bit audio compressed to 8-bit as specified by G.711
    pub fn enable_pcmu(mut self, enabled: bool, use_red: bool) -> Self {
        self.codec_config.enable_pcmu(enabled, use_red);
        self
    }

    /// Enable PCM a-law audio codec, optionally with RFC 2198 RED (see [`Self::enable_opus`] for
    /// `use_red`).
    ///
    /// This is 13-bit audio compressed to 8-bit as specified by G.711
    pub fn enable_pcma(mut self, enabled: bool, use_red: bool) -> Self {
        self.codec_config.enable_pcma(enabled, use_red);
        self
    }

    /// Enable G722 audio codec.
    ///
    /// This is 16 kHz audio compressed to 64 kbit/s as specified by G.722. Note that
    /// per RFC 3551 §4.5.2 the RTP clock rate on the wire is 8000 Hz even though the
    /// codec samples at 16 kHz. str0m treats G722 as a 16 kHz codec in the API and
    /// handles the 8 kHz RTP timestamp mapping internally. See
    /// <https://en.wikipedia.org/wiki/RTP_payload_formats#cite_note-55>
    ///
    /// See [`Self::enable_opus`] for `use_red`.
    pub fn enable_g722(mut self, enabled: bool, use_red: bool) -> Self {
        self.codec_config.enable_g722(enabled, use_red);
        self
    }

    /// Enable the RFC 3389 Comfort Noise payload at 8000 Hz.
    ///
    /// Disabled by default. Higher clock rates require a dynamic payload type
    /// and can be added through [`RtcConfig::codec_config`].
    pub fn enable_comfort_noise(mut self, enabled: bool) -> Self {
        self.codec_config.enable_comfort_noise(enabled);
        self
    }

    /// Enable AMR-WB audio codec.
    ///
    /// Disabled by default.
    pub fn enable_amr_wb(mut self, enabled: bool) -> Self {
        self.codec_config.enable_amr_wb(enabled);
        self
    }

    /// Enable VP8 video codec.
    ///
    /// Enabled by default.
    pub fn enable_vp8(mut self, enabled: bool) -> Self {
        self.codec_config.enable_vp8(enabled);
        self
    }

    /// Enable H264 video codec.
    ///
    /// Enabled by default.
    pub fn enable_h264(mut self, enabled: bool) -> Self {
        self.codec_config.enable_h264(enabled);
        self
    }

    /// Enable H265 video codec.
    ///
    /// Enabled by default.
    pub fn enable_h265(mut self, enabled: bool) -> Self {
        self.codec_config.enable_h265(enabled);
        self
    }

    /// Enable H266 (VVC) video codec.
    ///
    /// Disabled by default: no major browser (Chrome, Safari, Firefox)
    /// currently supports H.266/VVC, so it is opt-in.
    pub fn enable_h266(mut self, enabled: bool) -> Self {
        self.codec_config.enable_h266(enabled);
        self
    }

    /// Enable VP9 video codec.
    ///
    /// Enabled by default.
    pub fn enable_vp9(mut self, enabled: bool) -> Self {
        self.codec_config.enable_vp9(enabled);
        self
    }

    /// Enable AV1 video codec.
    ///
    /// Enabled by default.
    pub fn enable_av1(mut self, enabled: bool) -> Self {
        self.codec_config.enable_av1(enabled);
        self
    }

    /// Configure the RTP extension mappings.
    ///
    /// The default extension map is
    ///
    /// ```
    /// # use str0m::rtp::{Extension, ExtensionMap};
    /// let exts = ExtensionMap::standard();
    ///
    /// assert_eq!(exts.id_of(Extension::AudioLevel), Some(1));
    /// assert_eq!(exts.id_of(Extension::AbsoluteSendTime), Some(2));
    /// assert_eq!(exts.id_of(Extension::TransportSequenceNumber), Some(3));
    /// assert_eq!(exts.id_of(Extension::RtpMid), Some(4));
    /// assert_eq!(exts.id_of(Extension::RtpStreamId), Some(10));
    /// assert_eq!(exts.id_of(Extension::RepairedRtpStreamId), Some(11));
    /// assert_eq!(exts.id_of(Extension::VideoOrientation), Some(13));
    /// ```
    pub fn extension_map(&mut self) -> &mut ExtensionMap {
        &mut self.exts
    }

    /// Set the extension map replacing the existing.
    pub fn set_extension_map(mut self, exts: ExtensionMap) -> Self {
        self.exts = exts;
        self
    }

    /// Clear out the standard extension mappings.
    pub fn clear_extension_map(mut self) -> Self {
        self.exts.clear();

        self
    }

    /// Set an extension mapping on session level.
    ///
    /// The media level will be capped by the extension enabled on session level.
    ///
    /// The id must be 1-14 inclusive (1-indexed).
    pub fn set_extension(mut self, id: u8, ext: Extension) -> Self {
        self.exts.set(id, ext);
        self
    }

    /// Set the interval between statistics events.
    ///
    /// None turns off the stats events.
    ///
    /// This includes [`MediaEgressStats`][crate::stats::MediaEgressStats],
    /// [`MediaIngressStats`][crate::stats::MediaIngressStats]
    pub fn set_stats_interval(mut self, interval: Option<Duration>) -> Self {
        self.stats_interval = interval;
        self
    }

    /// The configured statistics interval.
    ///
    /// None means statistics are disabled.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// # use std::time::Duration;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to None.
    /// assert_eq!(config.stats_interval(), None);
    /// ```
    pub fn stats_interval(&self) -> Option<Duration> {
        self.stats_interval
    }

    /// Sets the interval between RTCP sender/receiver reports for audio streams.
    ///
    /// Defaults to 5 seconds.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is zero.
    pub fn set_rtcp_report_interval_audio(mut self, interval: Duration) -> Self {
        assert!(!interval.is_zero());
        self.intervals.audio = interval;
        self
    }

    /// Returns the interval between RTCP sender/receiver reports for audio streams.
    pub fn rtcp_report_interval_audio(&self) -> Duration {
        self.intervals.audio
    }

    /// Sets the interval between RTCP sender/receiver reports for video streams.
    ///
    /// Defaults to 1 second.
    ///
    /// # Panics
    ///
    /// Panics if `interval` is zero.
    pub fn set_rtcp_report_interval_video(mut self, interval: Duration) -> Self {
        assert!(!interval.is_zero());
        self.intervals.video = interval;
        self
    }

    /// Returns the interval between RTCP sender/receiver reports for video streams.
    pub fn rtcp_report_interval_video(&self) -> Duration {
        self.intervals.video
    }

    /// Enables estimation of available bandwidth (BWE).
    ///
    /// None disables the BWE. This is an estimation of the send bandwidth, not receive.
    ///
    /// This includes setting the initial estimate to start with.
    pub fn enable_bwe(mut self, initial_estimate: Option<Bitrate>) -> Self {
        match initial_estimate {
            Some(b) => {
                let conf = self.bwe_config.get_or_insert(BweConfig::new(b));
                conf.initial_bitrate = b;
            }
            None => {
                self.bwe_config = None;
            }
        }

        self
    }

    /// The initial bitrate as set by [`Self::enable_bwe()`].
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to None - BWE off.
    /// assert_eq!(config.bwe_initial_bitrate(), None);
    /// ```
    pub fn bwe_initial_bitrate(&self) -> Option<Bitrate> {
        self.bwe_config.as_ref().map(|c| c.initial_bitrate)
    }

    /// Sets the number of packets held back for reordering audio packets.
    ///
    /// Str0m tries to deliver the frames in order. This number determines how many
    /// packets to "wait" before releasing media
    /// [`contiguous: false`][crate::media::MediaData::contiguous].
    ///
    /// This setting is ignored in [RTP mode][`RtcConfig::set_rtp_mode()`] where RTP
    /// packets can arrive out of order.
    pub fn set_reordering_size_audio(mut self, size: usize) -> Self {
        self.reordering_size_audio = size;

        self
    }

    /// Returns the setting for audio reordering size.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to 15.
    /// assert_eq!(config.reordering_size_audio(), 15);
    /// ```
    ///
    /// This setting is ignored in [RTP mode][`RtcConfig::set_rtp_mode()`] where RTP
    /// packets can arrive out of order.
    pub fn reordering_size_audio(&self) -> usize {
        self.reordering_size_audio
    }

    /// Sets how long the oldest audio frame may wait to finish or for missing earlier packets.
    ///
    /// On expiry, an incomplete frame is dropped; a complete frame can move past a gap.
    /// Later frames stay buffered and retain their own deadlines. The default is 1 second.
    /// `None` disables this deadline and keeps count-based waiting only.
    /// Applies to audio streams in frame mode, not RTP mode.
    pub fn set_reordering_timeout_audio(mut self, timeout: Option<Duration>) -> Self {
        self.reordering_timeout_audio = timeout;
        self
    }

    /// Returns the configured audio reordering timeout.
    pub fn reordering_timeout_audio(&self) -> Option<Duration> {
        self.reordering_timeout_audio
    }

    /// Sets the number of packets held back for reordering video packets.
    ///
    /// Str0m tries to deliver the frames in order. This number determines how many
    /// packets to "wait" before releasing media with gaps.
    ///
    /// This must be at least as big as the number of packets the biggest keyframe
    /// can be split over.
    ///
    /// WARNING: video is very different to audio. Setting this value too low will result in
    /// missing video data. The 0 (as described for audio) is not relevant for video.
    ///
    /// Video recovery waits for a complete successor and missing packets' NACK attempts
    /// plus three RTTs. When NACK timing is unavailable, it waits three RTTs from the
    /// complete candidate's earliest receipt instead (100 ms RTT until measured).
    /// An incomplete frame without a complete successor has no age deadline.
    ///
    /// Default: 30
    ///
    /// This setting is ignored in [RTP mode][`RtcConfig::set_rtp_mode()`] where RTP
    /// packets can arrive out of order.
    pub fn set_reordering_size_video(mut self, size: usize) -> Self {
        self.reordering_size_video = size;

        self
    }

    /// Returns the setting for video reordering size.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to 30.
    /// assert_eq!(config.reordering_size_video(), 30);
    /// ```
    ///
    /// This setting is ignored in [RTP mode][`RtcConfig::set_rtp_mode()`] where RTP
    /// packets can arrive out of order.
    pub fn reordering_size_video(&self) -> usize {
        self.reordering_size_video
    }

    /// Sets how long an incoming stream may receive no packets before it is reported paused.
    /// Defaults to 1.5 seconds and applies to all incoming streams created by this `Rtc`.
    pub fn set_pause_threshold(mut self, threshold: Duration) -> Self {
        self.pause_threshold = threshold;
        self
    }

    /// Returns the incoming stream pause threshold.
    pub fn pause_threshold(&self) -> Duration {
        self.pause_threshold
    }

    /// Sets the buffer size for outgoing audio packets.
    ///
    /// This must be larger than 0. The value configures an internal ring buffer used as a temporary
    /// holding space between calling [`Writer::write`][crate::media::Writer::write()] and
    /// [`Rtc::poll_output`].
    ///
    /// For audio one call to `write()` typically results in one RTP packet since the entire
    /// payload fits in one. If you can guarantee that every `write()` is a single RTP packet,
    /// and is always followed by a `poll_output()`, it might be possible to set this value to 1.
    /// But that would give no margins for unexpected patterns.
    ///
    /// panics if set to 0.
    pub fn set_send_buffer_audio(mut self, size: usize) -> Self {
        assert!(size > 0);
        self.send_buffer_audio = size;
        self
    }

    /// Returns the setting for audio resend size.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to 50.
    /// assert_eq!(config.send_buffer_audio(), 50);
    /// ```
    pub fn send_buffer_audio(&self) -> usize {
        self.send_buffer_audio
    }

    /// Sets the buffer size for outgoing video packets and resends.
    ///
    /// This must be larger than 0. The value configures an internal ring buffer that is both
    /// used as a temporary holding space between calling
    /// [`Writer::write`][crate::media::Writer::write()] and [`Rtc::poll_output`] as well as for
    /// fulfilling resends.
    ///
    /// For video, this buffer is used for more than for audio. First, a call to `write()` often
    /// results in multiple RTP packets since large frames don't fit in one payload. That means
    /// the buffer must be at least as large to hold all those packets. Second, when the remote
    /// requests resends (NACK), those are fulfilled from this buffer. Third, for Bandwidth
    /// Estimation (BWE), when probing for available bandwidth, packets from this buffer are used
    /// to do "spurious resends", i.e. we do resends for packets that were not asked for.
    pub fn set_send_buffer_video(mut self, size: usize) -> Self {
        self.send_buffer_video = size;
        self
    }

    /// Returns the setting for video resend size.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to 1000.
    /// assert_eq!(config.send_buffer_video(), 1000);
    /// ```
    pub fn send_buffer_video(&self) -> usize {
        self.send_buffer_video
    }

    /// Sets how many bytes the data channels can buffer for sending, summed over all channels.
    ///
    /// [`Channel::write()`][crate::channel::Channel::write()] does not accept a message (returns
    /// `false`) that would take the total [buffered amount][crate::channel::Channel::buffered_amount()]
    /// above this limit. A message larger than the limit is never accepted.
    ///
    /// Written data counts as buffered until the remote peer acknowledges it, including after it
    /// has been sent. The limit therefore also caps the data in flight, and with it the data
    /// channel throughput at about this many bytes per round trip. The SCTP congestion window
    /// and the remote peer's advertised receive window can limit the data in flight further.
    ///
    /// Raising the limit helps a sender whose data rate times the round trip time exceeds it.
    /// Beyond what the network path holds, a higher limit only adds queueing delay, and accepted
    /// data is kept in memory until it is acknowledged.
    ///
    /// Defaults to 128 KiB.
    pub fn set_sctp_max_buffered_amount(mut self, bytes: usize) -> Self {
        self.sctp_max_buffered_amount = bytes;
        self
    }

    /// Returns how many bytes the data channels can buffer for sending.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to 128 KiB.
    /// assert_eq!(config.sctp_max_buffered_amount(), 128 * 1024);
    /// ```
    pub fn sctp_max_buffered_amount(&self) -> usize {
        self.sctp_max_buffered_amount
    }

    /// Sets the maximum data channel message size this peer can receive, in bytes.
    ///
    /// This limit is advertised in SDP as `a=max-message-size` and applied to SCTP,
    /// including when using the direct API or SNAP. It does not change the remote
    /// peer's receive limit or the local send buffer limit.
    ///
    /// The SCTP receive storage budget is at least 1 MiB and large enough to
    /// reassemble one message at this limit. Storage is allocated as data arrives.
    ///
    /// A value of 0 advertises unlimited message size, subject to local capacity.
    /// Internally, both the message limit and receive storage budget are `u32::MAX`.
    ///
    /// Defaults to 256 KiB.
    pub fn set_sctp_max_message_size(mut self, bytes: u32) -> Self {
        self.sctp_max_message_size = bytes;
        self
    }

    /// Returns the maximum data channel message size this peer can receive, in bytes.
    ///
    /// ```
    /// # use str0m::RtcConfig;
    /// let config = RtcConfig::new().set_sctp_max_message_size(512 * 1024);
    /// assert_eq!(config.sctp_max_message_size(), 512 * 1024);
    /// ```
    pub fn sctp_max_message_size(&self) -> u32 {
        self.sctp_max_message_size
    }

    /// Make the entire Rtc be in RTP mode.
    ///
    /// This means all media, read from [`RtpPacket`][crate::rtp::RtpPacket] and written to
    /// [`StreamTx::write_rtp`][crate::rtp::StreamTx::write_rtp] are RTP packetized.
    /// It bypasses all internal packetization/depacketization inside str0m.
    ///
    /// WARNING: This is a low level API and is not str0m's primary use case.
    pub fn set_rtp_mode(mut self, enabled: bool) -> Self {
        self.rtp_mode = enabled;

        self
    }

    /// Checks if RTP mode is set.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to false.
    /// assert_eq!(config.rtp_mode(), false);
    /// ```
    pub fn rtp_mode(&self) -> bool {
        self.rtp_mode
    }

    /// Enable the [`Event::RawPacket`][crate::Event::RawPacket] event.
    ///
    /// This clones data, and is therefore expensive.
    /// Should not be enabled outside of tests and troubleshooting.
    pub fn enable_raw_packets(mut self, enabled: bool) -> Self {
        self.enable_raw_packets = enabled;
        self
    }

    /// Enable SNAP (SCTP Negotiation Acceleration Protocol).
    ///
    /// When enabled, the `a=sctp-init` attribute is included in outgoing SDP
    /// offers, allowing the SCTP association to skip the 4-way handshake
    /// and go directly to established state. This saves up to two network
    /// round-trip times when establishing data channels.
    ///
    /// If the remote answer does not accept SNAP, str0m logs a warning and
    /// falls back to the regular SCTP handshake.
    ///
    /// When this is `false`, outgoing SDP offers will not include `a=sctp-init`.
    /// However, incoming offers that contain `a=sctp-init` will still be
    /// reciprocated in answers, since str0m always supports the extension at
    /// the protocol level (per §5.4 of draft-hancke-tsvwg-snap).
    ///
    /// See [draft-hancke-tsvwg-snap](https://datatracker.ietf.org/doc/draft-hancke-tsvwg-snap/).
    ///
    /// Default: `false`
    pub fn set_snap_enabled(mut self, enabled: bool) -> Self {
        self.snap_enabled = enabled;
        self
    }

    /// Set which DTLS version to use.
    ///
    /// Defaults to [`DtlsVersion::Dtls12`].
    pub fn set_dtls_version(mut self, version: DtlsVersion) -> Self {
        self.dtls_version = version;
        self
    }

    /// Get the configured DTLS version.
    pub fn dtls_version(&self) -> DtlsVersion {
        self.dtls_version
    }

    /// Set the MTU as a `target..=warn` inclusive range.
    ///
    /// * `*range.start()` is the **target** MTU: the size str0m aims for when
    ///   fragmenting outgoing packets. Must be within
    ///   `[DATAGRAM_MTU_TARGET_MIN, DATAGRAM_MTU_TARGET_MAX]`.
    /// * `*range.end()` is the **warn** threshold: packets whose length
    ///   exceeds the target trigger a warning.
    ///
    ///  Set as `target..=target` to warn packets bigger than target.
    ///  Set as `target..=usize::MAX` to disable warning.
    ///
    /// Defaults to `DATAGRAM_MTU_TARGET..=DATAGRAM_MTU_WARN`.
    pub fn set_mtu(mut self, range: RangeInclusive<usize>) -> Self {
        let (start, end) = (*range.start(), *range.end());
        assert!(
            DATAGRAM_MTU_TARGET_MIN <= start && start <= DATAGRAM_MTU_TARGET_MAX,
            "mtu target {} out of bounds [{}, {}]",
            start,
            DATAGRAM_MTU_TARGET_MIN,
            DATAGRAM_MTU_TARGET_MAX,
        );
        assert!(
            start <= end,
            "mtu range start must be <= end, got {}..={}",
            start,
            end,
        );
        self.mtu = range;
        self
    }

    /// Get the configured target MTU (the value str0m aims for).
    pub fn mtu(&self) -> usize {
        *self.mtu.start()
    }

    /// Get the configured warn threshold above which outgoing packets
    /// trigger a warning trace.
    pub fn mtu_warn(&self) -> usize {
        *self.mtu.end()
    }

    /// Set the VP9 packetizer mode.
    ///
    /// VP9 supports two RTP payload descriptor formats:
    ///
    /// * [`Vp9PacketizerMode::NonFlexible`] (default) — 5-6 byte header with layer indices
    ///   and scalability structure. Compatible with all major browsers including Safari.
    /// * [`Vp9PacketizerMode::Flexible`] — minimal 3-byte header. Simpler but may cause
    ///   issues with Safari which drops inter-frames.
    pub fn set_vp9_packetizer_mode(mut self, mode: Vp9PacketizerMode) -> Self {
        self.vp9_packetizer_mode = mode;
        self
    }

    /// Returns the configured VP9 packetizer mode.
    ///
    /// ```
    /// # use str0m::Rtc;
    /// # use str0m::format::Vp9PacketizerMode;
    /// let config = Rtc::builder();
    ///
    /// // Defaults to NonFlexible.
    /// assert_eq!(config.vp9_packetizer_mode(), Vp9PacketizerMode::NonFlexible);
    /// ```
    pub fn vp9_packetizer_mode(&self) -> Vp9PacketizerMode {
        self.vp9_packetizer_mode
    }

    /// Create a [`Rtc`] from the configuration.
    pub fn build(self, start: Instant) -> Rtc {
        Rtc::new_from_config(self, start).expect("Failed to create Rtc from config")
    }
}

impl BweConfig {
    fn new(initial_bitrate: Bitrate) -> Self {
        Self { initial_bitrate }
    }
}

impl Default for RtcConfig {
    fn default() -> Self {
        Self {
            local_ice_credentials: None,
            crypto_provider: None,
            dtls_cert: None,
            fingerprint_verification: true,
            ice_lite: false,
            initial_stun_rto: None,
            max_stun_rto: None,
            max_stun_retransmits: None,
            codec_config: CodecConfig::new_with_defaults(),
            exts: ExtensionMap::standard(),
            stats_interval: None,
            intervals: RtcpReportIntervals {
                audio: Duration::from_secs(5),
                video: Duration::from_secs(1),
            },
            bwe_config: None,
            reordering_size_audio: 15,
            reordering_size_video: 30,
            reordering_timeout_audio: Some(Duration::from_secs(1)),
            pause_threshold: Duration::from_millis(1500),
            send_buffer_audio: 50,
            send_buffer_video: 1000,
            sctp_max_buffered_amount: crate::sctp::DEFAULT_MAX_BUFFERED_AMOUNT,
            sctp_max_message_size: crate::sctp::LOCAL_MAX_MESSAGE_SIZE,
            rtp_mode: false,
            enable_raw_packets: false,
            dtls_version: DtlsVersion::Dtls12,
            vp9_packetizer_mode: Vp9PacketizerMode::default(),
            snap_enabled: false,
            mtu: DATAGRAM_MTU_TARGET..=DATAGRAM_MTU_WARN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sctp_max_message_size_round_trips() {
        assert_eq!(
            RtcConfig::default().sctp_max_message_size(),
            crate::sctp::LOCAL_MAX_MESSAGE_SIZE
        );
        for bytes in [0, 32 * 1024, 512 * 1024, u32::MAX] {
            let config = RtcConfig::new().set_sctp_max_message_size(bytes);
            assert_eq!(config.clone().sctp_max_message_size(), bytes);
        }
    }

    #[test]
    fn enable_comfort_noise_updates_codec_config() {
        let mut cfg = RtcConfig::default().enable_comfort_noise(true);

        assert!(
            cfg.codec_config()
                .params()
                .iter()
                .any(|p| p.spec().codec == crate::format::Codec::CN)
        );
    }

    #[test]
    fn default_mtu_is_datagram_mtu_target() {
        let cfg = RtcConfig::default();
        assert_eq!(cfg.mtu(), DATAGRAM_MTU_TARGET);
        assert_eq!(cfg.mtu_warn(), DATAGRAM_MTU_WARN);
    }

    #[test]
    fn set_mtu_round_trips() {
        let cfg = RtcConfig::default().set_mtu(900..=1400);
        assert_eq!(cfg.mtu(), 900);
        assert_eq!(cfg.mtu_warn(), 1400);

        let cfg = RtcConfig::default().set_mtu(900..=usize::MAX);
        assert_eq!(cfg.mtu(), 900);
        assert_eq!(cfg.mtu_warn(), usize::MAX);
    }

    #[test]
    fn set_mtu_accepts_equal_bounds() {
        let cfg = RtcConfig::default().set_mtu(1200..=1200);
        assert_eq!(cfg.mtu(), 1200);
        assert_eq!(cfg.mtu_warn(), 1200);

        let cfg = RtcConfig::default().set_mtu(DATAGRAM_MTU_TARGET_MIN..=DATAGRAM_MTU_TARGET_MIN);
        assert_eq!(cfg.mtu(), DATAGRAM_MTU_TARGET_MIN);

        let cfg = RtcConfig::default().set_mtu(DATAGRAM_MTU_TARGET_MAX..=DATAGRAM_MTU_TARGET_MAX);
        assert_eq!(cfg.mtu(), DATAGRAM_MTU_TARGET_MAX);
    }

    #[test]
    #[should_panic(expected = "mtu target")]
    fn set_mtu_panics_on_target_below_min() {
        let _ = RtcConfig::default().set_mtu((DATAGRAM_MTU_TARGET_MIN - 1)..=usize::MAX);
    }

    #[test]
    #[should_panic(expected = "mtu target")]
    fn set_mtu_panics_on_target_above_max() {
        let _ = RtcConfig::default().set_mtu((DATAGRAM_MTU_TARGET_MAX + 1)..=usize::MAX);
    }

    #[test]
    #[should_panic(expected = "mtu range start must be <= end")]
    fn set_mtu_panics_on_inverted_range() {
        let _ = RtcConfig::default().set_mtu(RangeInclusive::new(1300, 1299));
    }
}
