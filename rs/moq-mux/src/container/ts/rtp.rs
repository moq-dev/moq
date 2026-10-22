//! MPEG-TS over RTP (RFC 2250) for [`Export::with_rtp`](super::Export::with_rtp).
//!
//! Each datagram carries the [`PACKETS`] TS packets of one run of the synced multiplex's
//! timeline, and every header field is a function of that run and the broadcast: the sequence
//! number is the run's index and the timestamp its first packet's time on the PCR at 90 kHz,
//! each modulo its field's width, and the SSRC a hash of the broadcast's path and epoch. So two
//! legs of one broadcast send the same datagram, headers included, for every run both render
//! alike, and a receiver merging them (SMPTE ST 2022-7) can take each from either. A leg sends
//! only the runs it can vouch for: one holding a position another leg may fill otherwise
//! ([`Laid::unsure`](super::lane::Laid::unsure)) is withheld, so that leg's copy is the one
//! that arrives.

use std::sync::Arc;

use bytes::Bytes;
use mpeg2ts::ts::TsPacket;

use super::lane::Layout;

/// TS packets a datagram carries: the most a 1500-byte MTU takes (SMPTE ST 2022-2).
pub(super) const PACKETS: usize = 7;

/// RTP's static payload type for MPEG-2 TS (RFC 3551).
const PAYLOAD_TYPE: u8 = 33;

/// Version 2, no padding, extension or contributing sources (RFC 3550 5.1).
const VERSION: u8 = 0x80;

const HEADER: usize = 12;

/// Cuts a synced export's slots into RTP datagrams.
pub(super) struct Packetizer {
	layout: Arc<Layout>,
	ssrc: u32,
	/// The slot expected next: any other leaves the run in progress unfinished.
	next: Option<u128>,
	run: Option<Run>,
}

/// A datagram in progress.
struct Run {
	index: u128,
	packets: Vec<u8>,
	/// No position in it is one another leg may fill otherwise.
	sure: bool,
}

impl Packetizer {
	pub fn new(layout: Arc<Layout>, ssrc: u32) -> Self {
		Self {
			layout,
			ssrc,
			next: None,
			run: None,
		}
	}

	/// The datagrams slot `index`'s `payload` completes and this leg can vouch for, each with
	/// the position in the slot of its run's last packet. `unsure` lists the slot's positions,
	/// its clock at 0, another leg may fill otherwise.
	///
	/// A run is sent only whole: one the leg joined in the middle of, or that a jump in the
	/// slots cut short, is never sent, and neither is any stuffed out to a datagram's length.
	pub fn push(&mut self, index: u128, payload: &[u8], unsure: &[usize]) -> Vec<(usize, Bytes)> {
		if self.next != Some(index) {
			self.run = None;
		}
		self.next = Some(index + 1);
		let first = self.layout.first(index);
		let packets = payload.as_chunks::<{ TsPacket::SIZE }>().0;
		let mut sure = vec![true; packets.len()];
		for &position in unsure.iter().filter(|&&position| position < packets.len()) {
			sure[position] = false;
		}
		let mut out = Vec::new();
		for (position, packet) in packets.iter().enumerate() {
			let at = first + position as u128;
			let (index, offset) = (at / PACKETS as u128, at % PACKETS as u128);
			if offset == 0 {
				self.run = Some(Run {
					index,
					packets: Vec::with_capacity(PACKETS * TsPacket::SIZE),
					sure: true,
				});
			}
			let Some(run) = self.run.as_mut().filter(|run| run.index == index) else {
				continue;
			};
			run.packets.extend_from_slice(packet);
			run.sure &= sure[position];
			if offset + 1 == PACKETS as u128
				&& let Some(run) = self.run.take()
				&& run.sure
			{
				out.push((position, self.datagram(run)));
			}
		}
		out
	}

	fn datagram(&self, run: Run) -> Bytes {
		let mut out = Vec::with_capacity(HEADER + run.packets.len());
		out.extend_from_slice(&[VERSION, PAYLOAD_TYPE]);
		out.extend_from_slice(&(run.index as u16).to_be_bytes());
		out.extend_from_slice(&timestamp(&self.layout, run.index).to_be_bytes());
		out.extend_from_slice(&self.ssrc.to_be_bytes());
		out.extend_from_slice(&run.packets);
		Bytes::from(out)
	}
}

/// Run `index`'s RTP timestamp: when its first packet starts on the PCR, at 90 kHz, modulo
/// 2^32, as RFC 2250 stamps the target transmission time of a datagram's first byte.
fn timestamp(layout: &Layout, index: u128) -> u32 {
	(layout.time(index * PACKETS as u128) * 9 / 100_000) as u32
}

/// The SSRC of the broadcast at `path` from publisher instance `epoch`: FNV-1a over both, so
/// every leg derives the same one, and a replacement publisher or another path, barring a
/// 32-bit collision, another. The collision resolution of RFC 3550 8.2 is not applied: it
/// would have each leg pick a new SSRC of its own.
pub(super) fn ssrc(path: &str, epoch: &moq_net::Epoch) -> u32 {
	let mut hash: u32 = 0x811c_9dc5;
	for byte in path.bytes().chain([0]).chain(epoch.as_str().bytes()) {
		hash ^= u32::from(byte);
		hash = hash.wrapping_mul(0x0100_0193);
	}
	hash
}

#[cfg(test)]
mod test {
	use super::super::lane::Need;
	use super::*;

	/// 34 packets a slot, so runs straddle slots.
	const RATE: u64 = 34 * 188 * 8 * 40;

	fn layout() -> Arc<Layout> {
		Arc::new(Layout::new(RATE, 2, 0, &[(0x100, Need::Rest(u64::MAX))]).unwrap())
	}

	/// Slot `index`'s packets, each carrying its position on the timeline.
	fn slot(layout: &Layout, index: u128) -> Vec<u8> {
		let first = layout.first(index);
		(0..layout.count(index) as u128)
			.flat_map(|n| {
				let mut packet = [0xffu8; TsPacket::SIZE];
				packet[0] = 0x47;
				packet[4..20].copy_from_slice(&(first + n).to_be_bytes());
				packet
			})
			.collect()
	}

	fn positions(datagram: &[u8]) -> Vec<u128> {
		datagram[HEADER..]
			.chunks(TsPacket::SIZE)
			.map(|packet| u128::from_be_bytes(packet[4..20].try_into().unwrap()))
			.collect()
	}

	fn seq(datagram: &[u8]) -> u16 {
		u16::from_be_bytes([datagram[2], datagram[3]])
	}

	#[test]
	fn a_datagram_carries_one_run_of_the_timeline() {
		let layout = layout();
		let mut rtp = Packetizer::new(layout.clone(), 0x1234_5678);
		let mut sent = Vec::new();
		for index in 0..3 {
			let first = layout.first(index);
			sent.extend(
				rtp.push(index, &slot(&layout, index), &[])
					.into_iter()
					.map(|(last, datagram)| (first + last as u128, datagram)),
			);
		}
		// 102 packets: fourteen whole runs, and a fifteenth the next slot completes.
		assert_eq!(sent.len(), 14);
		for (n, (last, datagram)) in sent.iter().enumerate() {
			assert_eq!(datagram.len(), HEADER + PACKETS * TsPacket::SIZE);
			assert_eq!(&datagram[..2], [0x80, 33]);
			assert_eq!(seq(datagram), n as u16);
			assert_eq!(&datagram[8..12], 0x1234_5678u32.to_be_bytes());
			let first = (n * PACKETS) as u128;
			assert_eq!(
				positions(datagram),
				(first..first + PACKETS as u128).collect::<Vec<_>>()
			);
			assert_eq!(*last, first + PACKETS as u128 - 1);
			let at = u32::from_be_bytes(datagram[4..8].try_into().unwrap());
			assert_eq!(u128::from(at), layout.time(first) * 9 / 100_000);
		}
	}

	#[test]
	fn a_run_joined_in_its_middle_is_not_sent() {
		let layout = layout();
		let mut rtp = Packetizer::new(layout.clone(), 1);
		// Slot 1 opens at position 34, inside run 4.
		let sent = rtp.push(1, &slot(&layout, 1), &[]);
		assert_eq!(sent.iter().map(|(_, d)| seq(d)).collect::<Vec<_>>(), [5, 6, 7, 8]);
	}

	#[test]
	fn a_jump_leaves_the_run_it_cut_unsent() {
		let layout = layout();
		let mut rtp = Packetizer::new(layout.clone(), 1);
		rtp.push(0, &slot(&layout, 0), &[]);
		// Run 4 holds positions 28 to 34; slot 1, which carries position 34, never comes.
		let sent = rtp.push(2, &slot(&layout, 2), &[]);
		assert_eq!(sent.iter().map(|(_, d)| seq(d)).collect::<Vec<_>>(), [10, 11, 12, 13]);
	}

	#[test]
	fn a_run_holding_an_unsure_position_is_withheld() {
		let layout = layout();
		let mut rtp = Packetizer::new(layout.clone(), 1);
		let sent = rtp.push(0, &slot(&layout, 0), &[0, 15]);
		assert_eq!(sent.iter().map(|(_, d)| seq(d)).collect::<Vec<_>>(), [1, 3]);
		// Position 34 opens slot 1, so its doubt reaches back into run 4.
		let sent = rtp.push(1, &slot(&layout, 1), &[0]);
		assert_eq!(sent.iter().map(|(_, d)| seq(d)).collect::<Vec<_>>(), [5, 6, 7, 8]);
	}

	#[test]
	fn sequence_numbers_and_timestamps_wrap_at_their_widths() {
		let layout = layout();
		let mut rtp = Packetizer::new(layout.clone(), 1);
		// A slot whose runs cross a sequence wrap, and one whose time crosses a timestamp wrap.
		let seq_wrap = layout.slot_of(65_536 * PACKETS as u128);
		let sent = rtp.push(seq_wrap, &slot(&layout, seq_wrap), &[]);
		let seqs: Vec<u16> = sent.iter().map(|(_, d)| seq(d)).collect();
		assert!(seqs.windows(2).all(|w| w[1] == w[0].wrapping_add(1)), "{seqs:?}");
		assert!(seqs.contains(&0) && seqs.contains(&u16::MAX), "{seqs:?}");
		// 2^32 ticks at 90 kHz, in slots of 25 ms, which the PCR reaches a slot later.
		let ts_wrap = (1u128 << 32) * 1_000 / 90 / 25;
		let mut stamps = Vec::new();
		for index in ts_wrap - 1..ts_wrap + 3 {
			let sent = rtp.push(index, &slot(&layout, index), &[]);
			stamps.extend(
				sent.iter()
					.map(|(_, d)| u32::from_be_bytes(d[4..8].try_into().unwrap())),
			);
		}
		let steps: Vec<u32> = stamps.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
		// Seven packets at the rate take 5.15 ms.
		assert!(steps.iter().all(|step| (463..=464).contains(step)), "{steps:?}");
		assert!(
			stamps.windows(2).any(|w| w[1] < w[0]),
			"the timestamp wraps: {stamps:?}"
		);
	}

	#[test]
	fn the_ssrc_is_a_function_of_path_and_epoch() {
		let epoch: moq_net::Epoch = "0199f0c2-0000-7000-8000-000000000000".parse().unwrap();
		let other: moq_net::Epoch = "0199f0c2-0000-7000-8000-000000000001".parse().unwrap();
		// Pinned, so legs built from different releases still agree.
		assert_eq!(ssrc("live/one", &epoch), 0x04a6_c211);
		assert_ne!(ssrc("live/one", &epoch), ssrc("live/one", &other));
		assert_ne!(ssrc("live/one", &epoch), ssrc("live/two", &epoch));
	}
}
