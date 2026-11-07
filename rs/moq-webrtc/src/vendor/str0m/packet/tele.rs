//! RFC 4733 telephone-event (DTMF) RTP payloads.
//!
//! A payload is one or more 4-byte event reports and is never split across RTP packets.

use super::{CodecExtra, Depacketizer, PacketError, Packetizer};
use crate::rtp_::Frequency;
use std::time::Duration;

const REPORT_LEN: usize = 4;

/// A telephone-event (DTMF, RFC 4733) payload.
///
/// Builds and parses the 4-byte data carried in telephone-event RTP payloads.
/// The wire parsing and serialization methods are unversioned and take the negotiated RTP
/// clock rate on each call. This type is also available through `str0m::unversioned` when the
/// `unversioned` feature is enabled.
///
/// ```
/// use str0m::media::TelephoneEvent;
/// use str0m::media::Frequency;
/// use std::time::Duration;
///
/// let report = TelephoneEvent {
///     event: 5,
///     end: true,
///     volume: 10,
///     duration: Duration::from_millis(100),
/// };
/// let bytes = report.to_bytes(Frequency::EIGHT_KHZ).unwrap();
/// assert_eq!(TelephoneEvent::parse(&bytes, Frequency::EIGHT_KHZ), Some(report));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelephoneEvent {
    /// The event code (RFC 4733 Section 2.3.1).
    ///
    /// DTMF digits `0`-`9` are codes 0-9, `*` is 10, `#` is 11 and `A`-`D` are 12-15
    /// (RFC 4733 Section 3.2).
    pub event: u8,

    /// Whether this report ends the event (RFC 4733 Section 2.3.2).
    pub end: bool,

    /// The tone power in -dBm0, 0-63 (RFC 4733 Section 2.3.4). Larger values are quieter.
    pub volume: u8,

    /// How long the event has lasted so far (RFC 4733 Section 2.3.5).
    pub duration: Duration,
}

impl TelephoneEvent {
    /// Parses the report in the first four bytes of `buf`, using its RTP clock rate to convert
    /// the wire duration to [`Duration`].
    ///
    /// ## Unversioned API surface
    ///
    /// This method is not currently versioned according to semver rules.
    /// Breaking changes may be made in minor or patch releases.
    pub fn parse(buf: &[u8], clock_rate: Frequency) -> Option<Self> {
        let [event, flags, d0, d1, ..] = *buf else {
            return None;
        };

        Some(TelephoneEvent {
            event,
            end: flags & 0x80 != 0,
            volume: flags & 0x3f,
            duration: duration_from_units(u16::from_be_bytes([d0, d1]), clock_rate),
        })
    }

    /// Parses every report in an RTP payload.
    ///
    /// A payload can pack consecutive reports (RFC 4733 Section 2.5.1.5): the first starts at the
    /// RTP timestamp and each later one where the previous one ended. Returns `None` unless the
    /// payload is one or more whole reports. `clock_rate` is the payload type's RTP clock rate.
    ///
    /// ## Unversioned API surface
    ///
    /// This method is not currently versioned according to semver rules.
    /// Breaking changes may be made in minor or patch releases.
    pub fn parse_all(buf: &[u8], clock_rate: Frequency) -> Option<impl Iterator<Item = Self> + '_> {
        if buf.is_empty() || !buf.len().is_multiple_of(REPORT_LEN) {
            return None;
        }

        Some(
            buf.as_chunks::<REPORT_LEN>()
                .0
                .iter()
                .filter_map(move |report| Self::parse(report, clock_rate)),
        )
    }

    /// Serializes the report to its four wire bytes.
    ///
    /// The reserved bit is zero and only the low six bits of the volume are sent. Returns `None`
    /// if the duration exceeds the wire format's 16-bit field at `clock_rate`.
    ///
    /// ## Unversioned API surface
    ///
    /// This method is not currently versioned according to semver rules.
    /// Breaking changes may be made in minor or patch releases.
    pub fn to_bytes(&self, clock_rate: Frequency) -> Option<[u8; 4]> {
        let [d0, d1] = units_from_duration(self.duration, clock_rate)?.to_be_bytes();
        let end = if self.end { 0x80 } else { 0 };
        Some([self.event, end | (self.volume & 0x3f), d0, d1])
    }
}

pub(crate) fn duration_from_units(units: u16, clock_rate: Frequency) -> Duration {
    let nanos = (u128::from(units) * 1_000_000_000 + u128::from(clock_rate.get()) / 2)
        / u128::from(clock_rate.get());
    Duration::from_nanos(nanos as u64)
}

fn units_from_duration(duration: Duration, clock_rate: Frequency) -> Option<u16> {
    let scaled = duration
        .as_nanos()
        .checked_mul(u128::from(clock_rate.get()))?;
    u16::try_from((scaled + 500_000_000) / 1_000_000_000).ok()
}

#[derive(Debug)]
pub struct TelephoneEventPacketizer;

#[derive(Debug)]
pub struct TelephoneEventDepacketizer {
    pub(crate) clock_rate: Frequency,
}

impl Default for TelephoneEventDepacketizer {
    fn default() -> Self {
        Self {
            clock_rate: Frequency::EIGHT_KHZ,
        }
    }
}

impl Packetizer for TelephoneEventPacketizer {
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() {
            return Ok(vec![]);
        }

        if payload.len() > mtu {
            return Err(PacketError::ErrPayloadTooLarge);
        }

        Ok(vec![payload.to_vec()])
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, _last: bool) -> bool {
        false
    }

    // The marker bit flags the first packet of an event (RFC 4733 Section 2.2.2), which the
    // sender signals as a start of talkspurt.
    fn marks_talkspurt(&self) -> bool {
        true
    }

    fn nackable(&self) -> bool {
        false
    }
}

impl Depacketizer for TelephoneEventDepacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size.min(REPORT_LEN))
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        codec_extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        let mut reports = TelephoneEvent::parse_all(packet, self.clock_rate).ok_or(
            PacketError::InvalidTelephoneEvent(
                "payload must contain one or more complete 4-byte reports",
            ),
        )?;
        let first = reports
            .next()
            .expect("validated nonempty telephone payload");

        out.extend_from_slice(&packet[..REPORT_LEN]);
        *codec_extra = CodecExtra::Tele(first);
        Ok(())
    }

    fn is_partition_head(&self, _packet: &[u8]) -> bool {
        true
    }

    fn is_partition_tail(&self, _marker: bool, _packet: &[u8]) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // Event 5, end bit set, volume 10, duration 800.
    const REPORT: [u8; 4] = [0x05, 0x8a, 0x03, 0x20];
    // Event 6, volume 10, duration 320.
    const NEXT: [u8; 4] = [0x06, 0x0a, 0x01, 0x40];

    #[test]
    fn payload_roundtrip() {
        let report = TelephoneEvent {
            event: 5,
            end: true,
            volume: 10,
            duration: Duration::from_millis(100),
        };

        assert_eq!(report.to_bytes(Frequency::EIGHT_KHZ).unwrap(), REPORT);
        assert_eq!(
            TelephoneEvent::parse(&REPORT, Frequency::EIGHT_KHZ),
            Some(report)
        );
        for len in 0..REPORT_LEN {
            assert_eq!(
                TelephoneEvent::parse(&REPORT[..len], Frequency::EIGHT_KHZ),
                None
            );
        }
    }

    #[test]
    fn payload_duration_uses_rtp_clock_rate() {
        for clock_rate in [Frequency::EIGHT_KHZ, Frequency::FORTY_EIGHT_KHZ] {
            for units in [0, 1, 800, u16::MAX] {
                let duration = duration_from_units(units, clock_rate);
                assert_eq!(units_from_duration(duration, clock_rate), Some(units));
            }
        }

        let report = TelephoneEvent {
            event: 5,
            end: true,
            volume: 10,
            duration: duration_from_units(u16::MAX, Frequency::FORTY_EIGHT_KHZ),
        };
        let bytes = report.to_bytes(Frequency::FORTY_EIGHT_KHZ).unwrap();
        assert_eq!(&bytes[2..], &u16::MAX.to_be_bytes());
        assert_eq!(
            TelephoneEvent::parse(&bytes, Frequency::FORTY_EIGHT_KHZ),
            Some(report)
        );

        let too_long = TelephoneEvent {
            duration: Duration::from_secs(3),
            ..report
        };
        assert_eq!(too_long.to_bytes(Frequency::FORTY_EIGHT_KHZ), None);
    }

    #[test]
    fn payload_ignores_reserved_bit_and_masks_volume() {
        let report = TelephoneEvent::parse(&[0xff; 4], Frequency::EIGHT_KHZ).unwrap();
        assert_eq!(report.volume, 63);
        assert_eq!(
            report.to_bytes(Frequency::EIGHT_KHZ).unwrap(),
            [0xff, 0xbf, 0xff, 0xff]
        );

        let loud = TelephoneEvent {
            event: 0,
            end: false,
            volume: 0xff,
            duration: Duration::ZERO,
        };
        assert_eq!(
            loud.to_bytes(Frequency::EIGHT_KHZ).unwrap(),
            [0, 0x3f, 0, 0]
        );
    }

    #[test]
    fn parse_all_requires_whole_reports() {
        let packed = [REPORT, NEXT].concat();

        let reports: Vec<_> = TelephoneEvent::parse_all(&packed, Frequency::EIGHT_KHZ)
            .unwrap()
            .collect();
        assert_eq!(
            reports,
            [
                TelephoneEvent::parse(&REPORT, Frequency::EIGHT_KHZ).unwrap(),
                TelephoneEvent::parse(&NEXT, Frequency::EIGHT_KHZ).unwrap(),
            ]
        );

        for len in [0, 1, 3, 5, 7] {
            assert!(TelephoneEvent::parse_all(&packed[..len], Frequency::EIGHT_KHZ).is_none());
        }
    }

    #[test]
    fn packetizer_sends_reports_whole() {
        let mut packetizer = TelephoneEventPacketizer;
        let packed = [REPORT, NEXT].concat();

        assert_eq!(
            packetizer.packetize(4, &REPORT).unwrap(),
            vec![REPORT.to_vec()]
        );
        assert_eq!(packetizer.packetize(1200, &packed).unwrap(), vec![packed]);
    }

    #[test]
    fn packetizer_rejects_payload_larger_than_mtu() {
        let mut packetizer = TelephoneEventPacketizer;

        assert_eq!(
            packetizer.packetize(3, &REPORT),
            Err(PacketError::ErrPayloadTooLarge)
        );
    }

    #[test]
    fn packetizer_does_not_emit_empty_payload() {
        let mut packetizer = TelephoneEventPacketizer;

        assert!(packetizer.packetize(1200, &[]).unwrap().is_empty());
    }

    #[test]
    fn packetizer_marks_start_of_event_and_is_not_nackable() {
        let mut packetizer = TelephoneEventPacketizer;

        assert!(!packetizer.is_marker(&REPORT, None, true));
        assert!(packetizer.marks_talkspurt());
        assert!(!packetizer.nackable());
    }

    #[test]
    fn depacketizer_emits_one_report_at_a_time() {
        let mut depacketizer = TelephoneEventDepacketizer::default();
        let first = TelephoneEvent::parse(&REPORT, Frequency::EIGHT_KHZ).unwrap();
        let packed = [REPORT, NEXT].concat();

        for packet in [&REPORT[..], &packed[..]] {
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            depacketizer
                .depacketize(packet, &mut out, &mut extra)
                .unwrap();

            assert_eq!(out, REPORT);
            assert_eq!(extra, CodecExtra::Tele(first));
            assert!(depacketizer.is_partition_head(packet));
            assert!(depacketizer.is_partition_tail(false, packet));
        }
    }

    #[test]
    fn depacketizer_rejects_empty_and_partial_reports() {
        let mut depacketizer = TelephoneEventDepacketizer::default();
        let packed = [REPORT, NEXT].concat();

        for len in [0, 1, 3, 5, 7] {
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let result = depacketizer.depacketize(&packed[..len], &mut out, &mut extra);

            assert_eq!(
                result,
                Err(PacketError::InvalidTelephoneEvent(
                    "payload must contain one or more complete 4-byte reports"
                ))
            );
            assert!(out.is_empty());
            assert_eq!(extra, CodecExtra::None);
        }

        let error = PacketError::InvalidTelephoneEvent(
            "payload must contain one or more complete 4-byte reports",
        );
        assert_eq!(
            error.to_string(),
            "Invalid telephone event: payload must contain one or more complete 4-byte reports"
        );
    }
}
