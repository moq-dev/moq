//! Media stats a client reports about a broadcast, carried beside the
//! transport counters in a `moq-stats` entry.
//!
//! [`Stats`] is the [`moq_stats::Ext`] for media: a publisher reports what it
//! encoded and sent, a subscriber what it received and played. Both roles share
//! one type, so each fills only its own fields and the rest stay empty and off
//! the wire. Every field defaults when absent and unknown fields are ignored,
//! so readers and writers of different versions still understand each other.
//!
//! Counters are cumulative, like [`moq_stats::Traffic`], so a reader computes
//! rates from successive snapshots and the aggregate can sum clients. Gauges
//! (a latency, a bitrate, a round trip) describe one client right now and are
//! never summed. Durations are milliseconds and rates bits per second on the
//! wire.

use std::time::{Duration, SystemTime};

use moq_net::Timestamp;
use moq_net::bandwidth::Rate;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::{DeserializeAs, DurationMilliSecondsWithFrac, SerializeAs, TimestampMilliSeconds, serde_as};

/// What one media client reports for one broadcast.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Stats {
	/// The audio renditions.
	#[serde(skip_serializing_if = "Media::is_empty")]
	pub audio: Media,
	/// The video renditions.
	#[serde(skip_serializing_if = "Media::is_empty")]
	pub video: Media,
	/// A publisher's connection, sampled from its session. Repeated across the
	/// broadcasts one connection carries.
	#[serde(skip_serializing_if = "Transport::is_empty")]
	pub transport: Transport,
}

impl moq_stats::Merge for Stats {
	fn merge(&mut self, other: &Self) {
		self.audio.merge(&other.audio);
		self.video.merge(&other.video);
		self.transport.merge(&other.transport);
	}
}

impl moq_stats::Ext for Stats {}

/// One media kind's counters. A subscriber fills the receive and playback
/// fields; a publisher fills the send fields.
#[serde_as]
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Media {
	/// Subscriber: payload bytes received.
	#[serde(skip_serializing_if = "is_zero")]
	pub received_bytes: u64,
	/// Subscriber: frames received.
	#[serde(skip_serializing_if = "is_zero")]
	pub received_frames: u64,
	/// Subscriber: frames decoded.
	#[serde(skip_serializing_if = "is_zero")]
	pub decoded_frames: u64,
	/// Subscriber: frames dropped for arriving too late to play.
	#[serde(skip_serializing_if = "is_zero")]
	pub late_frames: u64,
	/// Subscriber: frames the decoder refused.
	#[serde(skip_serializing_if = "is_zero")]
	pub decode_errors: u64,
	/// Subscriber: times playback stalled waiting for media.
	#[serde(skip_serializing_if = "is_zero")]
	pub stalls: u64,
	/// Subscriber: total time spent stalled.
	#[serde_as(as = "DurationMilliSecondsWithFrac<f64>")]
	#[serde(skip_serializing_if = "Duration::is_zero")]
	pub stalled: Duration,
	/// Subscriber: times audio output ran dry.
	#[serde(skip_serializing_if = "is_zero")]
	pub underruns: u64,
	/// Subscriber: the newest media received and when it arrived, the
	/// broadcast's liveness as this client sees it.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub newest: Option<Arrival>,
	/// Subscriber gauge: how far playback trails the newest media received.
	#[serde_as(as = "Option<DurationMilliSecondsWithFrac<f64>>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub latency: Option<Duration>,

	/// Publisher: frames sent.
	#[serde(skip_serializing_if = "is_zero")]
	pub sent_frames: u64,
	/// Publisher: payload bytes sent.
	#[serde(skip_serializing_if = "is_zero")]
	pub sent_bytes: u64,
	/// Publisher: keyframes sent, a subset of `sent_frames`.
	#[serde(skip_serializing_if = "is_zero")]
	pub keyframes: u64,
	/// Publisher: frames dropped before they were encoded.
	#[serde(skip_serializing_if = "is_zero")]
	pub skipped_frames: u64,
	/// Publisher gauge: the encoder's target bitrate.
	#[serde_as(as = "Option<Bps>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bitrate: Option<Rate>,
}

impl Media {
	fn is_empty(&self) -> bool {
		*self == Self::default()
	}

	fn merge(&mut self, other: &Self) {
		self.received_bytes += other.received_bytes;
		self.received_frames += other.received_frames;
		self.decoded_frames += other.decoded_frames;
		self.late_frames += other.late_frames;
		self.decode_errors += other.decode_errors;
		self.stalls += other.stalls;
		self.stalled += other.stalled;
		self.underruns += other.underruns;
		self.sent_frames += other.sent_frames;
		self.sent_bytes += other.sent_bytes;
		self.keyframes += other.keyframes;
		self.skipped_frames += other.skipped_frames;
		if let Some(arrival) = other.newest
			&& self
				.newest
				.is_none_or(|newest| arrival.timestamp.as_micros() > newest.timestamp.as_micros())
		{
			self.newest = Some(arrival);
		}
	}
}

/// The newest media a subscriber received: exact in media time, and as
/// accurate as the subscriber's clock in wall time.
#[serde_as]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrival {
	/// The frame's presentation timestamp, in microseconds on the wire.
	#[serde_as(as = "Micros")]
	pub timestamp: Timestamp,
	/// When it arrived, as Unix milliseconds on the wire.
	#[serde_as(as = "TimestampMilliSeconds<i64>")]
	pub received: SystemTime,
}

/// A publisher's connection, sampled from [`moq_net::session::Stats`]. Every
/// field is absent when the transport does not report it.
#[serde_as]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Transport {
	/// Gauge: the smoothed round-trip time.
	#[serde_as(as = "Option<DurationMilliSecondsWithFrac<f64>>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rtt: Option<Duration>,
	/// Gauge: the congestion controller's estimated send rate.
	#[serde_as(as = "Option<Bps>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub send_rate: Option<Rate>,
	/// Bytes the connection lost.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bytes_lost: Option<u64>,
	/// Packets the connection lost.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub packets_lost: Option<u64>,
}

impl Transport {
	fn is_empty(&self) -> bool {
		*self == Self::default()
	}

	fn merge(&mut self, other: &Self) {
		let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
			(Some(a), Some(b)) => Some(a + b),
			(a, b) => a.or(b),
		};
		self.bytes_lost = sum(self.bytes_lost, other.bytes_lost);
		self.packets_lost = sum(self.packets_lost, other.packets_lost);
	}
}

impl From<moq_net::session::Stats> for Transport {
	fn from(stats: moq_net::session::Stats) -> Self {
		Self {
			rtt: stats.rtt,
			send_rate: stats.estimated_send_rate,
			bytes_lost: stats.bytes_lost,
			packets_lost: stats.packets_lost,
		}
	}
}

fn is_zero(value: &u64) -> bool {
	*value == 0
}

/// A [`Rate`] as bits per second.
struct Bps;

impl SerializeAs<Rate> for Bps {
	fn serialize_as<S: Serializer>(rate: &Rate, serializer: S) -> Result<S::Ok, S::Error> {
		rate.as_bps().serialize(serializer)
	}
}

impl<'de> DeserializeAs<'de, Rate> for Bps {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Rate, D::Error> {
		u64::deserialize(deserializer).map(Rate::from_bps)
	}
}

/// A [`Timestamp`] as whole microseconds.
struct Micros;

impl SerializeAs<Timestamp> for Micros {
	fn serialize_as<S: Serializer>(timestamp: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
		let micros = u64::try_from(timestamp.as_micros())
			.map_err(|_| serde::ser::Error::custom("a timestamp too large for microseconds"))?;
		micros.serialize(serializer)
	}
}

impl<'de> DeserializeAs<'de, Timestamp> for Micros {
	fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
		let micros = u64::deserialize(deserializer)?;
		Timestamp::from_micros(micros).map_err(serde::de::Error::custom)
	}
}

#[cfg(test)]
mod test {
	use moq_stats::{Merge, Stats as Entry, Traffic, TrafficFrame};

	use super::*;

	fn subscriber(decoded: u64, latency: u64, timestamp: u64) -> Stats {
		let mut stats = Stats::default();
		stats.video.decoded_frames = decoded;
		stats.video.stalled = Duration::from_millis(decoded);
		stats.video.latency = Some(Duration::from_millis(latency));
		stats.video.newest = Some(Arrival {
			timestamp: Timestamp::from_micros(timestamp).unwrap(),
			received: SystemTime::UNIX_EPOCH + Duration::from_millis(timestamp),
		});
		stats.transport.bytes_lost = Some(decoded);
		stats.transport.rtt = Some(Duration::from_millis(latency));
		stats
	}

	#[test]
	fn roundtrip_omits_the_other_role() {
		let stats = subscriber(30, 250, 1_000_000);
		let json = serde_json::to_string(&stats).unwrap();
		assert_eq!(
			json,
			concat!(
				r#"{"video":{"decoded_frames":30,"stalled":30.0,"#,
				r#""newest":{"timestamp":1000000,"received":1000000},"latency":250.0},"#,
				r#""transport":{"rtt":250.0,"bytes_lost":30}}"#
			)
		);
		assert_eq!(serde_json::from_str::<Stats>(&json).unwrap(), stats);
		assert_eq!(serde_json::to_string(&Stats::default()).unwrap(), "{}");
	}

	#[test]
	fn relay_frame_parses_with_media_defaulted() {
		let relay = r#"{"room":{"subscriptions_started":1,"subscriptions":1,"bytes":5}}"#;
		let frame: TrafficFrame<Stats> = serde_json::from_str(relay).unwrap();
		assert_eq!(frame["room"].traffic.bytes, 5);
		assert_eq!(frame["room"].ext, Stats::default());
	}

	#[test]
	fn media_frame_parses_as_plain_traffic() {
		let mut traffic = Traffic::default();
		traffic.bytes = 5;
		let entry = Entry {
			traffic,
			ext: subscriber(30, 250, 1_000_000),
		};
		let json = serde_json::to_string(&entry).unwrap();

		let plain: Traffic = serde_json::from_str(&json).unwrap();
		assert_eq!(plain, traffic, "an older reader ignores the media fields");
		let relay: Entry = serde_json::from_str(&json).unwrap();
		assert_eq!(relay.traffic, traffic);
		let media: Entry<Stats> = serde_json::from_str(&json).unwrap();
		assert_eq!(media, entry);
	}

	#[test]
	fn merge_sums_counters_and_leaves_gauges() {
		let mut total = subscriber(30, 250, 2_000_000);
		total.merge(&subscriber(10, 900, 1_000_000));

		assert_eq!(total.video.decoded_frames, 40);
		assert_eq!(total.video.stalled, Duration::from_millis(40));
		assert_eq!(total.transport.bytes_lost, Some(40));
		assert_eq!(total.video.latency, Some(Duration::from_millis(250)), "gauge untouched");
		assert_eq!(total.transport.rtt, Some(Duration::from_millis(250)), "gauge untouched");
		assert_eq!(
			total.video.newest.unwrap().timestamp,
			Timestamp::from_micros(2_000_000).unwrap(),
			"the newest arrival wins"
		);

		// Merging into an empty total takes the counters but no gauges.
		let mut empty = Stats::default();
		empty.merge(&subscriber(10, 900, 1_000_000));
		assert_eq!(empty.video.decoded_frames, 10);
		assert_eq!(empty.video.latency, None);
		assert_eq!(empty.transport.rtt, None);
		assert!(empty.video.newest.is_some(), "liveness is not a gauge");
	}
}
