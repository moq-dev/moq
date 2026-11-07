use std::time::{Duration, Instant};

use crate::vendor::str0m::rtp::SeqNo;
use crate::vendor::str0m::rtp_::{ReceptionReport, extend_u32};
use crate::vendor::str0m::util::value_history::ValueHistory;
use crate::vendor::str0m::util::{InstantExt, calculate_rtt};

use super::MidRid;

/// Holder of stats.
#[derive(Debug)]
pub(crate) struct StreamTxStats {
	/// count of bytes sent, including retransmissions
	/// <https://www.w3.org/TR/webrtc-stats/#dom-rtcsentrtpstreamstats-bytessent>
	pub bytes: u64,
	/// count of retransmitted bytes alone
	bytes_resent: u64,
	/// count of packets sent, including retransmissions
	/// <https://www.w3.org/TR/webrtc-stats/#summary>
	pub packets: u64,
	/// count of retransmitted packets alone
	packets_resent: u64,
	/// count of FIR requests received
	firs: u64,
	/// count of PLI requests received
	plis: u64,
	/// count of NACKs received
	nacks: u64,
	/// round trip time
	/// Can be null in case of missing or bad reports
	rtt: Option<Duration>,
	/// The last reception report for the stream, if any.
	///
	/// The SeqNo is the extended max_seq of the reception report, extended
	/// using the last sent sequence number.
	last_rr: Option<(SeqNo, ReceptionReport)>,

	/// `None` if `rtx_ratio_cap` is `None`.
	pub bytes_transmitted: Option<ValueHistory<u64>>,

	/// `None` if `rtx_ratio_cap` is `None`.
	pub bytes_retransmitted: Option<ValueHistory<u64>>,
}

impl StreamTxStats {
	pub fn new() -> Self {
		Self {
			bytes: 0,
			bytes_resent: 0,
			packets: 0,
			packets_resent: 0,
			firs: 0,
			plis: 0,
			nacks: 0,
			rtt: None,
			last_rr: None,
			bytes_transmitted: Some(Default::default()),
			bytes_retransmitted: Some(Default::default()),
		}
	}

	pub fn update_packet_counts(&mut self, bytes: u64, is_resend: bool) {
		self.packets += 1;
		self.bytes += bytes;
		if is_resend {
			self.bytes_resent += bytes;
			self.packets_resent += 1;
		}
	}

	pub fn increase_nacks(&mut self) {
		self.nacks += 1;
	}

	pub fn increase_plis(&mut self) {
		self.plis += 1;
	}

	pub fn increase_firs(&mut self) {
		self.firs += 1;
	}

	pub fn update_with_rr(&mut self, now: Instant, last_sent_seq_no: SeqNo, r: ReceptionReport) {
		let ntp_time = now.to_ntp_duration();
		let rtt = calculate_rtt(ntp_time, r.last_sr_delay, r.last_sr_time);
		self.rtt = rtt;

		// The last_sent_seq_no should be in the vicinity of the rr.max_seq.
		let ext_seq = extend_u32(Some(*last_sent_seq_no), r.max_seq).into();

		self.last_rr = Some((ext_seq, r));
	}
}
