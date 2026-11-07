use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, trace};

use crate::vendor::str0m::config_mod::RtcpReportIntervals;
use crate::vendor::str0m::media::KeyframeRequestKind;
use crate::vendor::str0m::rtp_::MidRid;
use crate::vendor::str0m::rtp_::SeqNoWindow;
use crate::vendor::str0m::rtp_::{Bitrate, extend_u32};
use crate::vendor::str0m::rtp_::{Fir, FirEntry, Frequency, MediaTime, Remb};
use crate::vendor::str0m::rtp_::{Mid, Pli, Pt, ReceiverReport};
use crate::vendor::str0m::rtp_::{ReportList, Rid, Rtcp};
use crate::vendor::str0m::rtp_::{RtcpFb, RtpHeader, SenderInfo, SeqNo};
use crate::vendor::str0m::rtp_::{SdesType, Ssrc};
use crate::vendor::str0m::util::{InstantExt, SystemTimeExt};
use crate::vendor::str0m::util::{already_happened, calculate_rtt};

use super::RtpPacket;
use super::StreamPaused;
use super::register::ReceiverRegister;

/// Incoming encoded stream.
///
/// A stream is a primary SSRC + optional RTX SSRC.
///
/// This is RTP level API. For frame level API see [`Rtc::writer`][crate::vendor::str0m::Rtc::writer].
#[derive(Debug)]
pub struct StreamRx {
	/// Unique idenfier of the remote stream.
	///
	/// If the remote changes the SSRC, we will create a new stream, not change this id.
	ssrc: Ssrc,

	/// Identifier of a resend (RTX) stream. This can be set later, once we discover it.
	rtx: Option<Ssrc>,

	/// Previous main SSRC. This is to ensure we never go "backwards" in terms
	/// of changing SSRC (for FF).
	previous_ssrc: Option<Ssrc>,

	/// The Media mid/rid this stream belongs to.
	midrid: MidRid,

	/// Incoming CNAME in Sdes reports.
	cname: Option<String>,

	/// Whether we explicitly want to supress NACK sending. This is normally done by not
	/// setting an RTX, however this can be toggled off manually despite RTX being there.
	///
	/// This is also set to true if the SDP negotiation disables RTX.
	///
	/// Defaults to false.
	suppress_nack: bool,

	/// Timestamp when we got some indication of remote using this stream.
	last_used: Instant,

	/// Last seen pt and clock_rate in
	last_clock_rate: Option<(Pt, Frequency)>,

	/// Last received sender info.
	sender_info: Option<LastSenderInfo>,

	/// ROC to reset with on next incoming packet.
	reset_roc: Option<u64>,

	/// Register of received packets. For NACK handling.
	///
	/// Set on first ever packet.
	register: Option<ReceiverRegister>,

	/// Sequence and replay tracking for RTX. NACKs and reception reports use the main register.
	///
	/// Set on the first authenticated RTX packet or restored from remembered sequence history.
	register_rtx: Option<SeqNoWindow>,

	/// Last observed media time in an RTP packet.
	last_time: Option<MediaTime>,

	/// If we have a pending keyframe request to send.
	pending_request_keyframe: Option<KeyframeRequestKind>,

	/// If we have a pending REMB request to send.
	pending_request_remb: Option<Bitrate>,

	/// Sequence number of the next FIR.
	fir_seq_no: u8,

	/// Last time we produced regular feedback RR.
	last_receiver_report: Instant,

	/// Statistics of incoming data.
	stats: StreamRxStats,

	/// When we need to evaluate the paused state.
	///
	/// now + pause_threshold
	check_paused_at: Option<Instant>,

	/// Whether we consider this StreamRx paused.
	///
	/// A stream is considered paused if it has received no packets for some (configurable) duration.
	/// This defaults to 1.5s.
	paused: bool,

	/// Whether we need to emit a paused event for the current paused state.
	need_paused_event: bool,

	/// The configured threshold before considering the lack of packets as going into paused.
	pause_threshold: Duration,
}

/// The last sender info we recieved.
#[derive(Debug)]
pub(crate) struct LastSenderInfo {
	/// When this SenderInfo was received.
	received_at: Instant,
	/// The sender info itself.
	info: SenderInfo,
	/// Whether we have emitted it yet via `poll_sender_info`
	emitted: bool,
}

/// Holder of stats.
#[derive(Debug, Default)]
pub(crate) struct StreamRxStats {
	/// count of bytes received, including retransmissions
	bytes: u64,
	/// count of packets received, including retransmissions
	packets: u64,
	/// count of FIR requests sent
	firs: u64,
	/// count of PLI requests sent
	plis: u64,
	/// count of NACKs sent
	nacks: u64,
	/// interarrival jitter (RTP timestamp units) from the last RR we sent
	jitter: u32,
	/// round trip time from the last DLRR, if any
	rtt: Option<Duration>,
	/// fraction of packets lost from the last RR, if any
	loss: Option<f32>,
}

impl StreamRx {
	pub(crate) fn new(ssrc: Ssrc, midrid: MidRid, suppress_nack: bool, pause_threshold: Duration) -> Self {
		debug!("Create StreamRx for SSRC: {}", ssrc);

		StreamRx {
			ssrc,
			rtx: None,
			previous_ssrc: None,
			midrid,
			cname: None,
			suppress_nack,
			last_used: already_happened(),
			last_clock_rate: None,
			sender_info: None,
			reset_roc: None,
			register: None,
			register_rtx: None,
			last_time: None,
			pending_request_keyframe: None,
			pending_request_remb: None,
			fir_seq_no: 0,
			last_receiver_report: already_happened(),
			stats: StreamRxStats::default(),
			check_paused_at: None,
			paused: true,
			need_paused_event: false,
			pause_threshold,
		}
	}

	/// The (primary) SSRC of this encoded stream.
	pub fn ssrc(&self) -> Ssrc {
		self.ssrc
	}

	/// The resend (RTX) SSRC of this encoded stream.
	pub fn rtx(&self) -> Option<Ssrc> {
		self.rtx
	}

	/// Mid for this stream.
	///
	/// In SDP this corresponds to m-line and "Media".
	pub fn mid(&self) -> Mid {
		self.midrid.mid()
	}

	/// Rid for this stream.
	///
	/// This is used to separate streams with the same [`Mid`] when using simulcast.
	pub fn rid(&self) -> Option<Rid> {
		self.midrid.rid()
	}

	/// CNAME as sent by remote peer in a Sdes.
	///
	/// The value is None until we receive a first report with the value set.
	pub fn cname(&self) -> Option<&str> {
		self.cname.as_deref()
	}

	/// Set threshold duration for emitting the paused event.
	///
	/// This event is emitted when no packet have received for this duration.
	pub fn set_pause_threshold(&mut self, t: Duration) {
		self.pause_threshold = t;
	}

	/// The last time we received a packet.
	///
	/// Resets if the SSRC changes.
	pub fn last_time(&self) -> Option<MediaTime> {
		self.last_time
	}

	/// Request a keyframe for an incoming encoded stream.
	///
	/// * SSRC the identifier of the remote encoded stream to request a keyframe for.
	/// * kind PLI or FIR.
	pub fn request_keyframe(&mut self, kind: KeyframeRequestKind) {
		self.pending_request_keyframe = Some(kind);
	}

	/// Request max recv bitrate for an incoming encoded stream.
	///
	/// * bitrate Bitrate.
	pub fn request_remb(&mut self, bitrate: Bitrate) {
		self.pending_request_remb = Some(bitrate);
	}

	/// Suppress NACK sending.
	///
	/// Normally NACK is disabled by not having an RTX SSRC set. In some situations it might be
	/// desirable to manually suppress NACK sending regardless of RTX setting.
	pub fn suppress_nack(&mut self, suppress: bool) {
		self.suppress_nack = suppress;
	}

	fn is_audio(&self) -> bool {
		self.rtx.is_none() // this is maybe not correct, but it's all we got.
	}

	pub(crate) fn receiver_report_at(&self, intervals: RtcpReportIntervals) -> Instant {
		self.last_receiver_report + intervals.for_audio(self.is_audio())
	}

	pub(crate) fn handle_rtcp(&mut self, now: Instant, fb: RtcpFb) {
		use RtcpFb::*;
		match fb {
			SenderInfo(v) => {
				self.set_sender_info(now, v);
			}
			SourceDescription(v) => {
				for (sdes, st) in v.values {
					if sdes == SdesType::CNAME {
						if st.is_empty() {
							// In simulcast, chrome doesn't send the SSRC lines, but
							// expects us to infer that from rtp headers. It does
							// however send the SourceDescription RTCP with an empty
							// string CNAME. ¯\_(ツ)_/¯
							return;
						}

						// Here we _could_ check CNAME here matches something. But
						// CNAMEs are a bit unfashionable.
						self.cname = Some(st);
						return;
					}
				}
			}
			Goodbye(_v) => {
				// We get Goodbye at weird times, like SDP renegotiation, which makes
				// pausing on the BYE not a good idea. Chrome also reuses the SSRC it
				// just sent BYE on. Very not helpful.
			}
			_ => {}
		}
	}

	fn set_sender_info(&mut self, now: Instant, mut info: SenderInfo) {
		// Extend the incoming time given our knowledge of last time.
		let extended = {
			let prev = self.sender_info.as_ref().map(|last| last.info.rtp_time.numer());
			let r_u32 = info.rtp_time.numer() as u32;
			extend_u32(prev, r_u32)
		};

		// The MediaTime has a base 1 after being parsed. At this point
		// we know whether it's audio or video and set the base accordingly.
		let clock_rate = self.last_clock_rate.map(|(_, r)| r).unwrap_or(Frequency::SECONDS);

		// Clock rate is that of the last received packet.
		info.rtp_time = MediaTime::new(extended, clock_rate);

		self.sender_info = Some(LastSenderInfo {
			received_at: now,
			info,
			emitted: false,
		});
	}

	pub(crate) fn paused_at(&self) -> Option<Instant> {
		self.check_paused_at
	}

	pub(crate) fn has_pending_request(&self) -> bool {
		self.pending_request_keyframe.is_some() || self.pending_request_remb.is_some()
	}

	pub(crate) fn handle_timeout(&mut self, now: Instant) {
		// No scheduled paused check?
		if self.check_paused_at.is_none() {
			return;
		}

		// Not reached scheduled paused check?
		if Some(now) < self.check_paused_at {
			return;
		}

		// Every update() schedules a paused check in the future. If we have reached that
		// future we have implicitly also paused.
		self.check_paused_at = None;

		self.paused = true;
		self.need_paused_event = true;
	}

	pub(crate) fn extend_seq(
		&mut self,
		header: &RtpHeader,
		is_repair: bool,
		max_seq_lookup: impl Fn(Ssrc) -> Option<SeqNo>,
	) -> SeqNo {
		// RTX has its own sequence series and only needs a replay window.
		if is_repair {
			if self.register_rtx.is_none() {
				self.register_rtx = max_seq_lookup(header.ssrc).map(SeqNoWindow::new_after);
			}
			return header.sequence_number(self.register_rtx.as_ref().map(|r| r.max_seq()));
		}

		let register = self
			.register
			.get_or_insert_with(|| ReceiverRegister::new(max_seq_lookup(header.ssrc)));

		// If the user has called `reset_seq_no`, this is the time to handle it, but only
		// if the incoming packet is for main (not repair).
		let mut reset_seq_no = None;
		if let Some(reset_roc) = self.reset_roc.take() {
			let s: SeqNo = (reset_roc << 16 | header.sequence_number as u64).into();
			reset_seq_no = Some(s);
		}

		if let Some(reset_seq_no) = reset_seq_no {
			reset_seq_no
		} else {
			header.sequence_number(register.max_seq())
		}
	}

	pub(crate) fn is_new_packet(&self, is_repair: bool, seq_no: SeqNo) -> bool {
		if is_repair {
			self.register_rtx.as_ref().is_none_or(|r| r.accepts(seq_no))
		} else {
			// extend_seq() initializes the main register before this check.
			self.register.as_ref().unwrap().accepts(seq_no)
		}
	}

	pub(crate) fn update_register(
		&mut self,
		now: Instant,
		header: &RtpHeader,
		clock_rate: Frequency,
		is_repair: bool,
		seq_no: SeqNo,
	) -> RegisterUpdateReceipt {
		self.last_used = now;

		let was_paused = self.paused;
		if was_paused {
			self.paused = false;
			self.need_paused_event = true;
		}
		self.check_paused_at = Some(now + self.pause_threshold);

		let is_new_packet = if is_repair {
			let is_new = self.is_new_packet(true, seq_no);
			self.register_rtx
				.get_or_insert_with(|| SeqNoWindow::new(seq_no))
				.update(seq_no);
			is_new
		} else {
			// extend_seq() initializes the main register before this update.
			self.register
				.as_mut()
				.unwrap()
				.update(seq_no, now, header.timestamp, clock_rate.get())
		};

		// Get the previous time for comparison
		let previous_time = self.last_time.map(|t| t.numer());

		// Calculate the extended timestamp
		let mut time_u32 = extend_u32(previous_time, header.timestamp);

		if was_paused && Some(time_u32) < previous_time {
			// In 32-bit RTP timestamps, adding 2^31 (MAX/2) flips to the other half of timestamp space
			// This forces extend_u32 to produce a value in the next cycle
			const HALF_CYCLE: u32 = 1u32 << 31;
			let adjusted_ts = header.timestamp.wrapping_add(HALF_CYCLE);

			// Recalculate extended timestamp with adjusted value
			let adjusted_time_u32 = extend_u32(previous_time, adjusted_ts);

			// If this adjusted timestamp moves time forward, use it
			if adjusted_time_u32 > previous_time.unwrap() {
				time_u32 = adjusted_time_u32;
			} else {
				// Fallback
				time_u32 = header.timestamp as u64;
			}
		}

		let time = MediaTime::new(time_u32, clock_rate);

		if !is_repair {
			self.last_time = Some(time);
		}

		RegisterUpdateReceipt { time, is_new_packet }
	}

	#[allow(clippy::too_many_arguments)]
	pub(crate) fn handle_rtp(
		&mut self,
		now: Instant,
		header: RtpHeader,
		payload: Arc<[u8]>,
		seq_no: SeqNo,
		time: MediaTime,
		wire_payload_len: usize,
	) -> RtpPacket {
		let packet = self.make_rtp_packet(now, header, payload, seq_no, time);

		// `payload` may be narrower than what was on the wire (RED unwrapped to its primary
		// block); count what was received, matching the sender's egress accounting.
		self.stats.bytes += wire_payload_len as u64;
		self.stats.packets += 1;

		packet
	}

	/// Build an [`RtpPacket`] WITHOUT counting it in receive stats. Used for RFC 2198
	/// FEC-recovered packets, which were never received on the wire (the carrying packet's
	/// bytes are already counted by `handle_rtp`).
	pub(crate) fn make_rtp_packet(
		&mut self,
		now: Instant,
		header: RtpHeader,
		payload: Arc<[u8]>,
		seq_no: SeqNo,
		time: MediaTime,
	) -> RtpPacket {
		trace!("Handle RTP: {:?}", header);

		let need_clock_rate = self.last_clock_rate.map(|(pt, _)| pt) != Some(header.payload_type);
		if need_clock_rate {
			self.last_clock_rate = Some((header.payload_type, time.frequency()));

			// If we get an SR before the first packet, we update the potential clock rate.
			if let Some(i) = &mut self.sender_info {
				i.info.rtp_time = MediaTime::new(i.info.rtp_time.numer(), time.frequency());
			}
		}

		RtpPacket {
			seq_no,
			time,
			header,
			payload,
			vp8_patch: None,
			nackable: false,
			last_sender_info: self.sender_info.as_ref().map(|l| l.info),
			timestamp: now,
		}
	}

	/// Rewrites the header to be the un-RTX'd original packet, and returns
	/// `data` with the original sequence number prefix stripped.
	pub(crate) fn un_rtx<'a>(&self, header: &mut RtpHeader, data: &'a [u8], pt: Pt) -> &'a [u8] {
		let mut orig_seq_no_16 = 0;

		let n = RtpHeader::read_original_sequence_number(data, &mut orig_seq_no_16);

		trace!("Repaired seq no {} -> {}", header.sequence_number, orig_seq_no_16);

		header.sequence_number = orig_seq_no_16;

		header.ssrc = self.ssrc;
		header.payload_type = pt;
		header.ext_vals.rid = header.ext_vals.rid_repair.take();

		&data[n..]
	}

	pub(crate) fn maybe_create_keyframe_request(&mut self, sender_ssrc: Ssrc, feedback: &mut VecDeque<Rtcp>) {
		let Some(kind) = self.pending_request_keyframe.take() else {
			return;
		};

		let ssrc = self.ssrc;

		match kind {
			KeyframeRequestKind::Pli => {
				self.stats.plis += 1;
				feedback.push_back(Rtcp::Pli(Pli { sender_ssrc, ssrc }))
			}
			KeyframeRequestKind::Fir => {
				self.stats.firs += 1;
				feedback.push_back(Rtcp::Fir(Fir {
					sender_ssrc,
					reports: FirEntry {
						ssrc,
						seq_no: self.next_fir_seq_no(),
					}
					.into(),
				}))
			}
		}
	}

	pub(crate) fn maybe_create_remb_request(&mut self, sender_ssrc: Ssrc, feedback: &mut VecDeque<Rtcp>) {
		let Some(bitrate) = self.pending_request_remb.take() else {
			return;
		};

		feedback.push_back(Rtcp::Remb(Remb {
			sender_ssrc,
			ssrc: 0.into(),
			bitrate: bitrate.as_f64() as f32,
			ssrcs: vec![*self.ssrc],
		}))
	}

	fn next_fir_seq_no(&mut self) -> u8 {
		let x = self.fir_seq_no;
		self.fir_seq_no = self.fir_seq_no.wrapping_add(1);
		x
	}

	pub(crate) fn need_rr(&self, now: Instant, intervals: RtcpReportIntervals) -> bool {
		if self.ssrc.is_probe() {
			return false;
		}

		now >= self.receiver_report_at(intervals)
	}

	pub(crate) fn create_rr_and_update(&mut self, now: Instant, sender_ssrc: Ssrc, feedback: &mut VecDeque<Rtcp>) {
		let mut rr = self.create_receiver_report(now);
		rr.sender_ssrc = sender_ssrc;

		if !rr.reports.is_empty() {
			let report = &rr.reports[rr.reports.len() - 1];
			self.stats.update_loss(report.fraction_lost);
			self.stats.jitter = report.jitter;
		}

		trace!("Created feedback RR ({:?}): {:?}", self.midrid, rr);
		feedback.push_back(Rtcp::ReceiverReport(rr));

		self.last_receiver_report = now;
	}

	fn create_receiver_report(&mut self, now: Instant) -> ReceiverReport {
		let Some(mut report) = self.register.as_mut().and_then(|r| r.reception_report()) else {
			return ReceiverReport {
				sender_ssrc: 0.into(), // set one level up
				reports: ReportList::new(),
			};
		};
		report.ssrc = self.ssrc;

		// The middle 32 bits out of 64 in the NTP timestamp (as explained in
		// Section 4) received as part of the most recent RTCP sender report
		// (SR) packet from source SSRC_n.  If no SR has been received yet,
		// the field is set to zero.
		report.last_sr_time = {
			let t64 = self.sender_info.as_ref().map_or(0u64, |l| l.info.ntp_time.as_ntp_64());

			(t64 >> 16) as u32
		};

		// The delay, expressed in units of 1/65_536 seconds, between
		// receiving the last SR packet from source SSRC_n and sending this
		// reception report block.  If no SR packet has been received yet
		// from SSRC_n, the DLSR field is set to zero.
		report.last_sr_delay = if let Some(l) = self.sender_info.as_ref() {
			let t = l.received_at;
			let delay = now - t;
			((delay.as_micros() * 65_536) / 1_000_000) as u32
		} else {
			0
		};

		ReceiverReport {
			sender_ssrc: 0.into(), // set one level up
			reports: report.into(),
		}
	}

	pub(crate) fn recovery_deadline(&self, range: std::ops::Range<SeqNo>, received: Instant) -> Option<Instant> {
		if !self.suppress_nack {
			if let Some(register) = &self.register {
				return register.recovery_deadline(range, received, self.stats.rtt);
			}
		}
		let rtt = self.stats.rtt.unwrap_or(super::register_nack::DEFAULT_RTT);
		received.checked_add(rtt.saturating_mul(3))
	}

	/// Whether this stream should keep its NACK timer active.
	///
	/// Paused streams finish pending retries before disabling NACK reports.
	pub(crate) fn nack_enabled(&self) -> bool {
		// Deliberately don't look at RTX is_some() here, since when using dynamic SSRC, we might need
		// to send NACK before discovering the remote RTX.
		if self.suppress_nack {
			return false;
		}

		if !self.paused {
			return true;
		}

		let Some(register) = &self.register else {
			return false;
		};

		register.has_pending_nacks()
	}

	pub(crate) fn maybe_create_nack(
		&mut self,
		now: Instant,
		sender_ssrc: Ssrc,
		feedback: &mut VecDeque<Rtcp>,
	) -> Option<()> {
		if !self.nack_enabled() {
			return None;
		}

		let nacks = self
			.register
			.as_mut()
			.and_then(|r| r.nack_report(now, self.stats.rtt))?;

		for mut nack in nacks {
			nack.sender_ssrc = sender_ssrc;
			nack.ssrc = self.ssrc;

			trace!("Created feedback NACK: {:?}", nack);
			feedback.push_back(Rtcp::Nack(nack));
			self.stats.nacks += 1;
		}

		Some(())
	}

	pub(crate) fn poll_paused(&mut self) -> Option<StreamPaused> {
		if self.ssrc.is_probe() {
			return None;
		}

		if !self.need_paused_event {
			return None;
		}

		self.need_paused_event = false;

		debug!(
			"{} StreamRx with {:?} and SSRC: {}",
			if self.paused { "Paused" } else { "Unpaused" },
			self.midrid,
			self.ssrc
		);

		Some(StreamPaused {
			ssrc: self.ssrc,
			mid: self.midrid.mid(),
			rid: self.midrid.rid(),
			paused: self.paused,
		})
	}

	/// Poll the most recent sender info and when it was received
	pub(crate) fn poll_sender_info(&mut self) -> Option<(SenderInfo, Instant)> {
		let i = self.sender_info.as_mut()?;
		if i.emitted {
			return None;
		}

		i.emitted = true;

		Some((i.info, i.received_at))
	}

	pub(crate) fn reset_buffers(&mut self, max_seq_lookup: impl Fn(Ssrc) -> Option<SeqNo>) {
		if let Some(r) = &mut self.register {
			r.clear(max_seq_lookup(self.ssrc));
		}

		if self.register_rtx.is_some() {
			self.register_rtx = self.rtx.and_then(max_seq_lookup).map(SeqNoWindow::new_after);
		}
		self.pending_request_keyframe = None;
	}

	#[must_use]
	pub(crate) fn change_ssrc(&mut self, ssrc: Ssrc) -> bool {
		// Avoid flapping
		if ssrc == self.ssrc || Some(ssrc) == self.previous_ssrc {
			return false;
		}

		debug!("Change main SSRC: {} -> {} {:?}", self.ssrc, ssrc, self.midrid);

		// Remember which was the previous in case a stray packet turns up
		// so do we don't go "backwards".
		self.previous_ssrc = Some(self.ssrc);
		self.ssrc = ssrc;

		// Reset all SSRC-specific state
		self.register = None;
		self.last_time = None;
		self.last_clock_rate = None;
		self.sender_info = None;
		self.last_receiver_report = already_happened();
		self.fir_seq_no = 0;
		self.pending_request_keyframe = None;
		self.pending_request_remb = None;
		self.reset_roc = None;

		// Note: We don't reset the RTX register here, as the RTX SSRC is managed separately
		// via maybe_reset_rtx() and is not directly tied to the main SSRC change.

		true
	}

	pub(crate) fn maybe_reset_rtx(&mut self, rtx: Ssrc) {
		if let Some(current) = self.rtx {
			if current == rtx {
				return;
			}

			debug!(
				"Change RTX SSRC {} -> {} for main SSRC: {} {:?}",
				current, rtx, self.ssrc, self.midrid
			);
		} else {
			debug!("SSRC {} associated with RTX: {}", self.ssrc, rtx);
		}

		self.rtx = Some(rtx);
		self.register_rtx = None;
	}

	/// Reset the current rollover counter (ROC).
	///
	/// This is used in scenarios where we use a single sequence number across all
	/// receivers of the same stream (as opposed to a sequence number unique per peer).
	///
	/// The ROC is not carried in the RTP header. Obtain it through out-of-band
	/// signaling and call this before handling the first SRTP packet for the stream.
	/// See [`RtpWrite::new`][crate::vendor::str0m::rtp::RtpWrite::new] for sender and receiver examples.
	///
	/// [RFC3711](https://datatracker.ietf.org/doc/html/rfc3711#section-3.3.1):
	///
	/// > Receivers joining an on-going session MUST be given the
	/// > current ROC value using out-of-band signaling such as key-management
	/// > signaling.  Furthermore, the receiver SHALL initialize s_l to the RTP
	/// > sequence number (SEQ) of the first observed SRTP packet (unless the
	/// > initial value is provided by out of band signaling such as key
	/// > management).
	pub fn reset_roc(&mut self, roc: u64) {
		self.register = None;
		self.register_rtx = None;
		self.reset_roc = Some(roc);
	}

	pub(crate) fn is_midrid(&self, midrid: MidRid) -> bool {
		midrid.special_equals(&self.midrid)
	}
}

impl StreamRxStats {
	fn update_loss(&mut self, fraction_lost: u8) {
		self.loss = Some(fraction_lost as f32 / u8::MAX as f32)
	}
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegisterUpdateReceipt {
	pub time: MediaTime,
	pub is_new_packet: bool,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn paused_timestamp_repair_moves_time_forward() {
		let now = already_happened();
		let mut stream = StreamRx::new(7.into(), MidRid("mid".into(), None), false, Duration::from_millis(1500));
		let previous_time = 1;
		stream.last_time = Some(MediaTime::new(previous_time, Frequency::NINETY_KHZ));
		stream.paused = true;

		let header = RtpHeader {
			payload_type: Pt::new_with_value(96),
			sequence_number: 42,
			timestamp: 0,
			ssrc: 7.into(),
			..Default::default()
		};

		let seq_no = stream.extend_seq(&header, false, |_| None);
		let receipt = stream.update_register(now, &header, Frequency::NINETY_KHZ, false, seq_no);

		assert!(
			receipt.time.numer() > previous_time,
			"expected repaired media time to move forward"
		);
		assert!(!stream.paused);
	}

	/// Feed packets `(seq, rtp_time)` into a fresh stream, as received on the wire.
	fn stream_with(packets: &[(u16, u32)]) -> StreamRx {
		let now = already_happened();
		let mut stream = StreamRx::new(7.into(), MidRid("mid".into(), None), false, Duration::from_millis(1500));
		for (seq, ts) in packets {
			let header = RtpHeader {
				payload_type: Pt::new_with_value(111),
				sequence_number: *seq,
				timestamp: *ts,
				ssrc: 7.into(),
				..Default::default()
			};
			let seq_no = stream.extend_seq(&header, false, |_| None);
			stream.update_register(now, &header, Frequency::FORTY_EIGHT_KHZ, false, seq_no);
		}
		stream
	}

	#[test]
	fn nack_retries_use_receive_stream_rtt() {
		for rtt in [Duration::from_millis(20), Duration::from_millis(250)] {
			let mut stream = stream_with(&[(10, 0), (12, 1920)]);
			stream.stats.rtt = Some(rtt);
			let now = Instant::now();
			let mut feedback = VecDeque::new();
			assert!(stream.maybe_create_nack(now, 1.into(), &mut feedback).is_some());
			let due = now + rtt + Duration::from_millis(5);
			assert!(
				stream
					.maybe_create_nack(due - Duration::from_nanos(1), 1.into(), &mut feedback)
					.is_none()
			);
			assert_eq!(feedback.len(), 1);
			assert!(stream.maybe_create_nack(due, 1.into(), &mut feedback).is_some());
			assert_eq!(feedback.len(), 2);
		}
	}

	#[test]
	fn paused_stream_finishes_pending_nack_retries() {
		let mut stream = stream_with(&[(10, 0), (12, 1920)]);
		let mut now = stream.paused_at().unwrap();
		stream.handle_timeout(now);
		assert!(stream.paused);

		let mut feedback = VecDeque::new();
		for _ in 0..5 {
			assert!(stream.nack_enabled());
			assert!(stream.maybe_create_nack(now, 1.into(), &mut feedback).is_some());
			assert!(stream.maybe_create_nack(now, 1.into(), &mut feedback).is_none());
			now += Duration::from_millis(105);
		}
		assert_eq!(feedback.len(), 5);
		assert!(
			feedback.iter().all(|rtcp| {
				matches!(rtcp, Rtcp::Nack(nack) if nack.reports.len() == 1 && nack.reports[0].pid == 11)
			})
		);

		assert!(!stream.nack_enabled());
		assert!(stream.maybe_create_nack(now, 1.into(), &mut feedback).is_none());
		assert_eq!(feedback.len(), 5);
	}

	#[test]
	fn receiver_report_uses_supplied_intervals() {
		let mut stream = StreamRx::new(7.into(), MidRid("mid".into(), None), false, Duration::from_millis(1500));
		let now = Instant::now();
		stream.last_receiver_report = now;

		assert_eq!(
			stream.receiver_report_at(RtcpReportIntervals {
				audio: Duration::from_millis(750),
				video: Duration::from_millis(500),
			}),
			now + Duration::from_millis(750)
		);
	}

	#[test]
	fn rtx_tracks_rollover_and_reordering_only_after_receipt() {
		let mut stream = rtx_stream();
		let header = rtx_header(8, 50000);
		stream.extend_seq(&header, true, |_| None);
		assert!(
			stream.register_rtx.is_none(),
			"unverified headers must not advance the window"
		);

		for seq in [65535, 65536, 65538, 65537] {
			record_rtx(&mut stream, seq);
			assert!(!stream.is_new_packet(true, seq.into()));
		}
		assert!(
			stream.register.is_none(),
			"RTX must not update primary reception reports"
		);
		let mut feedback = VecDeque::new();
		assert!(
			stream
				.maybe_create_nack(Instant::now(), 1.into(), &mut feedback)
				.is_none()
		);
		assert!(feedback.is_empty());
	}

	#[test]
	fn rtx_uses_the_full_128_position_window() {
		let mut stream = rtx_stream();
		record_rtx(&mut stream, 0);
		record_rtx(&mut stream, 200);
		assert!(!stream.is_new_packet(true, 72.into()));
		assert!(stream.is_new_packet(true, 73.into()));
		record_rtx(&mut stream, 73);
		assert!(!stream.is_new_packet(true, 73.into()));
	}

	#[test]
	fn rtx_buffer_and_roc_resets_preserve_remembered_replay_floor() {
		let mut stream = rtx_stream();
		record_rtx(&mut stream, 65535);
		record_rtx(&mut stream, 65537);
		assert!(stream.is_new_packet(true, 65536.into()));
		let history = |ssrc| (ssrc == 8.into()).then_some(65537.into());
		stream.reset_buffers(history);
		assert!(!stream.is_new_packet(true, 65536.into()));
		assert!(!stream.is_new_packet(true, 65537.into()));

		stream.reset_roc(5);
		let header = rtx_header(8, 2);
		assert_eq!(stream.extend_seq(&header, true, history), 65538.into());
		assert!(!stream.is_new_packet(true, 65536.into()));
		assert!(stream.is_new_packet(true, 65538.into()));
		// The requested ROC applies to the main stream, not the RTX sequence series.
		let main = RtpHeader {
			ssrc: 7.into(),
			sequence_number: 10,
			..Default::default()
		};
		assert_eq!(stream.extend_seq(&main, false, history), ((5u64 << 16) + 10).into());
	}

	#[test]
	fn rtx_ssrc_changes_restore_history_and_main_changes_preserve_it() {
		let mut stream = rtx_stream();
		record_rtx(&mut stream, 65535);
		assert!(stream.change_ssrc(9.into()));
		assert!(!stream.is_new_packet(true, 65535.into()));
		stream.maybe_reset_rtx(8.into());
		assert!(!stream.is_new_packet(true, 65535.into()));

		stream.maybe_reset_rtx(10.into());
		assert!(stream.register_rtx.is_none());
		record_rtx(&mut stream, 0);
		stream.maybe_reset_rtx(8.into());
		let header = rtx_header(8, 0);
		let seq = stream.extend_seq(&header, true, |ssrc| (ssrc == 8.into()).then_some(65535.into()));
		assert_eq!(seq, 65536.into());
		assert!(!stream.is_new_packet(true, 65534.into()));
		assert!(!stream.is_new_packet(true, 65535.into()));
		assert!(stream.is_new_packet(true, seq));
		record_rtx(&mut stream, 65536);
	}

	fn rtx_stream() -> StreamRx {
		let mut stream = StreamRx::new(7.into(), MidRid("mid".into(), None), false, Duration::from_millis(1500));
		stream.maybe_reset_rtx(8.into());
		stream
	}

	fn record_rtx(stream: &mut StreamRx, seq: u64) {
		let header = rtx_header(*stream.rtx().unwrap(), seq as u16);
		let extended = stream.extend_seq(&header, true, |_| None);
		assert_eq!(extended, seq.into());
		assert!(stream.is_new_packet(true, extended));
		let receipt = stream.update_register(already_happened(), &header, Frequency::NINETY_KHZ, true, extended);
		assert!(receipt.is_new_packet);
	}

	fn rtx_header(ssrc: u32, seq: u16) -> RtpHeader {
		RtpHeader {
			ssrc: ssrc.into(),
			sequence_number: seq,
			payload_type: 97.into(),
			..Default::default()
		}
	}
}
