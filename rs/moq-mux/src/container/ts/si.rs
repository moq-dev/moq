//! Standalone SI carriage over snapshot tracks.
//!
//! Import captures each `(PID, table_id)` pair onto its own MoQ track, mapped from
//! the catalog's `mpegts.si` section ([`catalog::SiEntry`]). The track format is
//! snapshot-shaped: every group is a complete picture of the table's current
//! sections, one frame per sub-table, each frame the sub-table's sections
//! concatenated verbatim in `section_number` order. Frames apply in order and a
//! later frame replaces an earlier one with the same sub-table identity, so a
//! writer may append incremental updates within a group; this writer never does
//! (every group is a full set), but readers honor the rule so that stays a writer
//! choice rather than a format change. A joiner reads only the newest group.
//!
//! [`Capture`] is the import half: it buffers each sub-table's sections until a
//! complete generation is assembled, commits the generation atomically, and cuts a
//! new snapshot group, so a torn multi-section transition (half old version, half
//! new) is unrepresentable on the track. [`Snapshot`] is the export half: it
//! reduces a track's frames back into the current section set.
//!
//! An import that selects a program ([`Capture::select`]) carries SI for that
//! service alone. Other services' EIT actual sub-tables are dropped on arrival, and
//! each SDT actual snapshot is rebuilt as one section holding only the selected
//! service's entry, with a fresh CRC. An SDT actual section whose own CRC fails is
//! dropped, and a revision commits only once every section has arrived, so the last
//! good snapshot stays in force until then or until the source stops repeating it
//! for its table's maximum interval on the program clock ([`Clock`]). An SDT that does not list the
//! service carries no SDT actual, so a revision that drops it retires the earlier
//! one. Network-wide tables (NIT, BAT, SDT other, EIT other, TDT/TOT) pass through
//! verbatim, as does everything when no program is selected.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use kio::clock::Instant;

use bytes::Bytes;
use moq_net::Timestamp;

use super::catalog;
use super::mux_rate::{MAX_INTERVAL, PCR_WRAP};
use super::passthrough::PCR_HZ;
use super::psi::{self, CRC};

const SDT_PID: u16 = 0x0011;
const SDT_ACTUAL: u8 = 0x42;
const EIT_PID: u16 = 0x0012;

/// Coalesce snapshot cuts: a junction revises many sub-tables inside a second
/// (every service's now/next rolls on the hour), and one group carrying all of them
/// beats a burst of near-identical groups. Measured on the host clock, not the
/// media clock: bursts arrive in real time, and an audio-only stream never
/// advances the media clock at all (which would wedge every revision behind the
/// debounce forever). The first snapshot is never delayed.
const DEBOUNCE: Duration = Duration::from_secs(1);

/// A sub-table's identity within its `(PID, table_id)` entry: what a revised
/// generation replaces.
///
/// Generic section syntax carries `table_id_extension`; for two documented table
/// families that alone is ambiguous, so the spec-fixed scope bytes join the key
/// (see [`scope`]). A section with no long-form header has no extension, version,
/// or numbering, so its table is a single latest-value slot: each arrival replaces
/// the last. That is exactly right for the short-form tables DVB defines (TDT/TOT
/// are clocks; ST is stuffing receivers discard), and it is what lets a time table
/// be proxied at all (#2914): content-keyed identity would mint a fresh key per
/// tick. A hypothetical multi-section *static* short-form table would collapse to
/// its most recent section and alternate; the debounce bounds that at one small
/// group per second, which is the acceptable cost of not parsing tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Identity {
	/// A long-form sub-table: `table_id_extension` plus any table-specific scope.
	Sub { ext: u16, scope: u32 },
	/// A short-form or runt section: one slot for the whole table, latest wins.
	Table,
}

/// Compute a section's sub-table identity.
pub(super) fn identity(section: &[u8]) -> Identity {
	if !long_form(section) {
		return Identity::Table;
	}
	let ext = u16::from_be_bytes([section[3], section[4]]);
	Identity::Sub {
		ext,
		scope: scope(section),
	}
}

/// The table-specific bytes that extend the generic identity, for the two table
/// families whose `table_id_extension` alone is ambiguous (#2842).
///
/// These are fixed offsets of published ETSI EN 300 468 layouts, gated on
/// `table_id`; reading them narrows *identity*, not what export claims to
/// understand (the sections stay opaque bytes).
fn scope(section: &[u8]) -> u32 {
	match section[0] {
		// SDT other: the extension is a transport_stream_id, unique only within a
		// network; original_network_id at bytes 8..10 disambiguates.
		0x46 if section.len() >= 10 => u32::from(u16::from_be_bytes([section[8], section[9]])),
		// EIT: the extension is a service_id, unique only within a transport stream;
		// transport_stream_id at 8..10 and original_network_id at 10..12 disambiguate.
		0x4E..=0x6F if section.len() >= 12 => u32::from_be_bytes([section[8], section[9], section[10], section[11]]),
		_ => 0,
	}
}

/// Whether the section has a complete long-form header (syntax indicator set and
/// the 8 header bytes present).
fn long_form(section: &[u8]) -> bool {
	section.len() >= 8 && section[1] & 0x80 != 0
}

/// Split a frame payload back into its individual sections (they are
/// self-delimiting via `section_length`). Stops at the first malformed or
/// truncated header.
pub(super) fn split_sections(payload: &Bytes) -> Vec<Bytes> {
	let mut out = Vec::new();
	let mut off = 0;
	while payload.len() >= off + 3 {
		let len = 3 + ((((payload[off + 1] & 0x0f) as usize) << 8) | payload[off + 2] as usize);
		if payload.len() < off + len {
			break;
		}
		out.push(payload.slice(off..off + len));
		off += len;
	}
	out
}

/// Rebuild an SDT sub-table as one section listing only `service`, wherever its
/// entry sat among `sections`. The header (transport_stream_id, version_number,
/// original_network_id) is kept, the numbering reset to section 0 of 0, and the
/// length and CRC recomputed. `None` when no section lists the service.
fn sdt_service<'a>(sections: impl IntoIterator<Item = &'a Bytes>, service: u16) -> Option<Vec<u8>> {
	for section in sections {
		// The service loop runs from the 11-byte header to the CRC.
		let end = section.len().saturating_sub(4);
		let mut off = 11;
		while off + 5 <= end {
			let len = 5 + ((usize::from(section[off + 3] & 0x0f) << 8) | usize::from(section[off + 4]));
			if off + len > end {
				break;
			}
			if u16::from_be_bytes([section[off], section[off + 1]]) == service {
				let mut out = section[..11].to_vec();
				out.extend_from_slice(&section[off..off + len]);
				let section_length = out.len() - 3 + 4;
				out[1] = (out[1] & 0xf0) | (section_length >> 8) as u8;
				out[2] = section_length as u8;
				out[6] = 0;
				out[7] = 0;
				out.extend_from_slice(&CRC.checksum(&out).to_be_bytes());
				return Some(out);
			}
			off += len;
		}
	}
	None
}

/// One committed generation of a sub-table: the sections currently in force.
///
/// Keyed by `section_number`, sparse on purpose: DVB EIT schedule segments skip
/// unused section numbers, so 0..=last is not a contiguous range there.
#[derive(Default, PartialEq)]
struct Generation {
	/// The program-clock deadline for this generation's next repetition.
	expires: Duration,
	version: Option<u8>,
	sections: BTreeMap<u8, Bytes>,
}

/// A generation still being assembled, section by section.
#[derive(Default)]
struct Pending {
	expires: Duration,
	/// Highest section number this generation can hold, from `last_section_number`
	/// (also raised by any observed `section_number`, so a malformed header cannot
	/// hide a section).
	last: u8,
	sections: BTreeMap<u8, Bytes>,
}

/// How a generation was judged complete, deciding whether the commit replaces or
/// merges (see [`Entry::commit`]).
enum Complete {
	/// Every section of the declaration is present.
	Contiguous,
	/// The transmission cycle wrapped: complete as observed, bounded by the
	/// highest `last_section_number` seen.
	Wrapped { last: u8 },
}

impl Pending {
	/// All of 0..=last present: the fast path for densely-numbered tables (SDT, NIT,
	/// EIT now/next), committing the moment the last section lands. EIT schedule
	/// never satisfies this: its numbering is deliberately sparse (a real 8-day
	/// guide declares `last_section_number` 248 and transmits 32 sections), so the
	/// cycle-wrap path in [`Entry::section`] is its *only* commit path, not a
	/// fallback.
	fn contiguous(&self) -> bool {
		self.sections.len() == self.last as usize + 1
	}
}

/// One `(PID, table_id)` entry: its snapshot track and the state of every
/// sub-table riding it.
struct Entry {
	/// Created with the first snapshot that carries anything, so a table filtered
	/// to nothing never gets a track.
	track: Option<moq_net::track::Producer>,
	interval: Option<Duration>,
	/// The selected service an SDT actual snapshot is reduced to.
	service: Option<u16>,
	active: BTreeMap<Identity, Generation>,
	/// In-flight generations, keyed by sub-table and version. Tree nodes follow
	/// live generations instead of retaining a hash table's peak bucket allocation.
	pending: BTreeMap<(Identity, u8), Pending>,
	/// One deadline per retained generation: None is active, Some(version) is pending.
	/// Renewal replaces its old key rather than leaving stale timer entries behind.
	expiry: BTreeSet<(Duration, Identity, Option<u8>)>,
	/// A commit changed `active` since the last cut.
	dirty: bool,
	/// The newest snapshot group carries at least one sub-table.
	populated: bool,
	/// The entry appears in the catalog's `mpegts.si` map: exactly while it is
	/// `populated`, so a subscriber never finds an empty track.
	advertised: bool,
	/// Host-clock instant of the last cut ([`DEBOUNCE`]); `None` until the first.
	last_cut: Option<Instant>,
}

impl Entry {
	/// Fold one completed, current section into the store, committing its
	/// generation when it completes. Returns nothing; a commit marks the entry
	/// dirty and [`Capture::flush`] cuts the group.
	fn section(&mut self, section: Bytes, now: Duration) {
		self.expire(now);
		let id = identity(&section);
		let expires = now + self.interval.unwrap_or(Duration::from_secs(30));

		let Identity::Sub { .. } = id else {
			if let Some(current) = self.active.get(&id) {
				self.expiry.remove(&(current.expires, id, None));
			}
			self.expiry.insert((expires, id, None));
			// Latest-value slot: a short-form table has no version to buffer under, so
			// each arrival replaces the last (a TDT ticks ~every 30s and every section
			// is new bytes). A byte-identical repetition is not a change.
			let changed = self
				.active
				.get(&id)
				.is_none_or(|current| current.sections.get(&0) != Some(&section));
			if changed {
				self.active.insert(
					id,
					Generation {
						expires,
						version: None,
						sections: BTreeMap::from([(0u8, section)]),
					},
				);
				self.dirty = true;
			} else if let Some(current) = self.active.get_mut(&id) {
				current.expires = expires;
			}
			return;
		};

		let version = (section[5] >> 1) & 0x1f;
		let number = section[6];
		let last = section[7];

		// A byte-identical repetition of the current state is the overwhelmingly
		// common case (SI repeats every couple of seconds); recognize it before
		// touching any pending state.
		if let Some(current) = self.active.get_mut(&id)
			&& current.version == Some(version)
			&& current.sections.get(&number) == Some(&section)
		{
			self.expiry.remove(&(current.expires, id, None));
			current.expires = expires;
			self.expiry.insert((expires, id, None));
			return;
		}

		let pending = self.pending.entry((id, version)).or_default();
		self.expiry.remove(&(pending.expires, id, Some(version)));
		pending.expires = expires;
		self.expiry.insert((expires, id, Some(version)));
		pending.last = pending.last.max(last).max(number);

		if let Some(prior) = pending.sections.get(&number) {
			if *prior == section {
				// A reduced SDT reads a missing service as removal, so a dense SDT
				// missing a section (dropped for a bad CRC, or lost) must not commit as
				// complete-as-observed; it waits for the cycle that completes it.
				if self.service.is_some() {
					return;
				}
				// The same section came round again before the set completed: the
				// transmission cycle wrapped, so what we hold is the whole sub-table as
				// this mux transmits it. For EIT schedule this is the *only* commit
				// path (see [`Pending::contiguous`]). The limit: a section lost before
				// the wrap is indistinguishable from a legitimately skipped number, so
				// the committed set is complete-as-observed; the same-version merge in
				// [`Self::commit`] fills it in on the next cycle. No section-counting
				// receiver can do better; version mixing is still unrepresentable.
				let last = pending.last;
				let sections = std::mem::take(&mut pending.sections);
				self.commit(id, version, sections, Complete::Wrapped { last });
				return;
			}
			// Same version, new bytes: non-conformant (a revision must bump the
			// version), but taking the newer bytes degrades more gracefully than
			// pinning the older ones.
			pending.sections.insert(number, section);
			return;
		}

		pending.sections.insert(number, section);
		if pending.contiguous() {
			let sections = std::mem::take(&mut pending.sections);
			self.commit(id, version, sections, Complete::Contiguous);
		}
	}

	/// Commit the sub-table's generation atomically and retire everything pending
	/// for it.
	///
	/// A contiguous commit replaces the old generation wholesale: it holds every
	/// section its declaration promises, so anything else active is stale, even
	/// under the same version number (versions are five bits, so a reception gap
	/// can bring the same value back with different content). A wrap commit is
	/// complete-as-observed, not complete, so it *merges* into a same-version
	/// active generation instead: a section lost before the cycle wrapped
	/// short-circuits as a repetition on the next cycle and the wrap only ever
	/// carries the stragglers, so replacing would flip-flop between incomplete
	/// subsets forever while merging converges. The merge is bounded by the new
	/// declaration's `last_section_number`, so a shrunken table cannot resurrect
	/// sections past its own end. Repetition of an unchanged set is not a change.
	fn commit(&mut self, id: Identity, version: u8, mut sections: BTreeMap<u8, Bytes>, complete: Complete) {
		let expires = self.pending[&(id, version)].expires;
		// Versions are five bits. Only this identity's versions can leave, so a
		// commit never scans unrelated in-flight sub-tables.
		for version in 0..32 {
			if let Some(pending) = self.pending.remove(&(id, version)) {
				self.expiry.remove(&(pending.expires, id, Some(version)));
			}
		}
		if let Some(current) = self.active.get(&id) {
			self.expiry.remove(&(current.expires, id, None));
		}
		self.expiry.insert((expires, id, None));
		if let Complete::Wrapped { last } = complete
			&& let Some(current) = self.active.get(&id)
			&& current.version == Some(version)
		{
			for (number, section) in current.sections.iter() {
				if *number <= last {
					sections.entry(*number).or_insert_with(|| section.clone());
				}
			}
		}
		let changed = self.active.get(&id).is_none_or(|current| current.sections != sections);
		self.active.insert(
			id,
			Generation {
				expires,
				version: Some(version),
				sections,
			},
		);
		if changed {
			self.dirty = true;
		}
	}

	/// Forget generations the source stopped repeating on its program clock.
	fn expire(&mut self, now: Duration) {
		while let Some(&(expires, id, version)) = self.expiry.first() {
			if expires >= now {
				break;
			}
			self.expiry.pop_first();
			if let Some(version) = version {
				self.pending.remove(&(id, version));
			} else {
				self.active.remove(&id);
				self.dirty = true;
				// Retraction is due on the program clock even during fast replay;
				// the host-clock debounce may only delay ordinary updates.
				self.last_cut = None;
			}
		}
	}

	/// Cut a snapshot group: the complete active set, one frame per sub-table, an
	/// SDT reduced to the selected service. Nothing is cut until a snapshot carries
	/// something, and that snapshot creates the entry's track on `broadcast`.
	/// `pts` stamps the frames; `now` is the host clock for the debounce.
	fn cut(
		&mut self,
		broadcast: &moq_net::broadcast::Producer,
		(pid, table_id): (u16, u8),
		pts: Timestamp,
		now: Instant,
	) -> anyhow::Result<()> {
		let frames: Vec<Vec<u8>> = self
			.active
			.values()
			.filter_map(|generation| match self.service {
				Some(service) => sdt_service(generation.sections.values(), service),
				None => Some(generation.sections.values().flatten().copied().collect()),
			})
			.collect();
		self.dirty = false;
		self.populated = !frames.is_empty();
		let track = match &mut self.track {
			Some(track) => track,
			None if frames.is_empty() => return Ok(()),
			// Deterministic, greppable name; fall back to a unique suffix on the
			// (pathological) collision with an existing track.
			None => {
				let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
				self.track.insert(
					match broadcast.create_track(format!("{pid:#06x}-{table_id:#04x}.si"), info.clone()) {
						Ok(track) => track,
						Err(_) => broadcast.unique_track(".si", info)?,
					},
				)
			}
		};
		let mut group = track.append_group()?;
		for payload in frames {
			group.write_frame(pts, payload)?;
		}
		group.finish()?;
		self.last_cut = Some(now);
		Ok(())
	}
}

/// The program clock SI ages on, admitted as whole PES are placed: SI before the first
/// PCR, or past it with no interval to extrapolate from, is dropped, so a source cannot
/// hold SI by withholding its clock.
///
/// A valid PCR interval (forward, at most [`MAX_INTERVAL`]) advances it by its own ticks and
/// joins the measured multiplex rate. Any other, a stalled or rewound PCR, a corrupt jump, or
/// one across a clock break, advances it by the input packets it spans at the measured rate.
/// So does the interval still open past the last PCR, once it outruns any valid one: until
/// then the next PCR may yet close it, and a burst above the average rate must not retire SI
/// early. A valid interval that closes a longer open one keeps what the open one charged, so
/// PCR silence ages SI whatever step resumes it.
/// The rate pools every valid interval rather than reading the catalog's `mux_rate`, which
/// a variable-rate source leaves unset.
#[derive(Default)]
struct Clock {
	/// Ticks elapsed through the last PCR.
	elapsed: u64,
	/// The last PCR (`None` past a clock break) and the input packet count at it.
	last: Option<(Option<u64>, u64)>,
	/// Ticks and packets over every valid interval.
	measured: (u64, u64),
}

impl Clock {
	/// A PCR on the program's clock PID, carried by input packet `count`.
	fn pcr(&mut self, pcr: u64, count: u64) {
		if let Some((last, at)) = self.last {
			let packets = count - at;
			let extrapolated = self.extrapolate(packets).unwrap_or(0);
			let step = last
				.map(|last| (pcr + PCR_WRAP - last) % PCR_WRAP)
				.filter(|step| (1..=MAX_INTERVAL).contains(step));
			let ticks = match step {
				// What [`Self::at`] already charged the open interval stays charged, or a
				// small step after a long silence would freeze SI for as long as it lasted.
				Some(step) => {
					let open = extrapolated.saturating_sub(MAX_INTERVAL);
					self.measured.0 += step;
					self.measured.1 += packets;
					step.max(open)
				}
				None => extrapolated,
			};
			self.elapsed = self.elapsed.saturating_add(ticks);
		}
		self.last = Some((Some(pcr), count));
	}

	/// The clock broke or changed PID: the interval across it is extrapolated.
	fn discontinuity(&mut self) {
		if let Some((pcr, _)) = &mut self.last {
			*pcr = None;
		}
	}

	/// Ticks `packets` span at the measured rate, or `None` before a valid interval.
	fn extrapolate(&self, packets: u64) -> Option<u64> {
		let (ticks, total) = self.measured;
		(total > 0)
			.then(|| u64::try_from(u128::from(packets) * u128::from(ticks) / u128::from(total)).unwrap_or(u64::MAX))
	}

	/// The clock at input packet `count`, or `None` when it cannot place SI there.
	fn at(&self, count: u64) -> Option<Duration> {
		let (_, at) = self.last?;
		let open = self.extrapolate(count - at)?.saturating_sub(MAX_INTERVAL);
		let ticks = self.elapsed.saturating_add(open);
		Some(Duration::from_secs(ticks / PCR_HZ) + Duration::from_nanos((ticks % PCR_HZ) * 1_000 / 27))
	}
}

/// Import-side SI capture: reassembled sections in, snapshot tracks and catalog
/// entries out.
///
/// Owned by the TS importer, which feeds it every completed section on a captured
/// SI PID and every PCR, and calls [`flush`](Self::flush) once per decode batch.
/// Dropping it removes the advertised entries from the catalog, mirroring the verbatim
/// tracks.
pub(super) struct Capture<E: catalog::Catalog> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,
	entries: BTreeMap<(u16, u8), Entry>,
	clock: Clock,
	/// The latest [`Clock`] reading, independent of host time and media PTS. Monotonic:
	/// an open interval's extrapolation may run ahead of the PCR that closes it.
	now: Duration,
	/// The selected program_number, which is the service_id of the only service
	/// carried; `None` captures every service.
	program: Option<u16>,
}

impl<E: catalog::Catalog> Capture<E> {
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: crate::catalog::Producer<E>) -> Self {
		Self {
			broadcast,
			catalog,
			entries: BTreeMap::new(),
			clock: Clock::default(),
			now: Duration::ZERO,
			program: None,
		}
	}

	/// A PCR on the program's clock PID, carried by input packet `count`.
	pub fn pcr(&mut self, pcr: u64, count: u64) {
		self.clock.pcr(pcr, count);
		self.advance(count);
	}

	/// The program clock broke or changed PID.
	pub fn discontinuity(&mut self) {
		self.clock.discontinuity();
	}

	fn advance(&mut self, count: u64) -> bool {
		let Some(now) = self.clock.at(count) else {
			return false;
		};
		self.now = self.now.max(now);
		true
	}

	/// Carry only the service numbered `program`: its EIT actual, and an SDT actual
	/// listing it alone. SI keys services by `service_id`, assumed equal to the PAT
	/// `program_number`.
	pub fn select(&mut self, program: u16) {
		self.program = Some(program);
	}

	/// Fold one completed section from `pid`, completed by input packet `count`, into its
	/// `(pid, table_id)` entry, creating the entry on first sight of the pair.
	///
	/// Returns `false` when the program clock cannot place it ([`Clock`]): it is dropped,
	/// for the caller to count unplaced.
	pub fn section(&mut self, pid: u16, section: Bytes, count: u64) -> bool {
		let Some(&table_id) = section.first() else {
			return true;
		};
		// A next-version section (current_next_indicator clear) describes a future
		// state, not the current one; carrying it would need a second generation slot
		// per sub-table for a table nothing re-emits, so it is dropped until the
		// version becomes current.
		if long_form(&section) && section[5] & 0x01 == 0 {
			return true;
		}
		// EIT actual (now/next and schedule) is keyed by service_id: another
		// service's sub-table describes a program this import does not carry.
		if let Some(program) = self.program
			&& pid == EIT_PID
			&& matches!(table_id, 0x4E | 0x50..=0x5F)
			&& !matches!(identity(&section), Identity::Sub { ext, .. } if ext == program)
		{
			return true;
		}

		let service = self.program.filter(|_| (pid, table_id) == (SDT_PID, SDT_ACTUAL));
		// The rebuilt SDT gets a fresh CRC, which would certify corruption the
		// source's CRC flags; dropping the section keeps the last good generation.
		if service.is_some() && !psi::crc_ok(&section) {
			return true;
		}
		if !self.advance(count) {
			return false;
		}
		let entry = self.entries.entry((pid, table_id)).or_insert_with(|| Entry {
			track: None,
			interval: catalog::si_interval(table_id),
			service,
			active: BTreeMap::new(),
			pending: BTreeMap::new(),
			expiry: BTreeSet::new(),
			dirty: false,
			populated: false,
			advertised: false,
			last_cut: None,
		});
		entry.section(section, self.now);
		true
	}

	/// Cut snapshot groups for the dirty entries, then advertise each entry whose
	/// snapshot now carries something and retract each whose snapshot went empty
	/// (one catalog lock for the whole batch).
	///
	/// `pts` is the media clock, used to stamp the frames; the cut debounce runs on
	/// the host clock ([`DEBOUNCE`]), and a first snapshot is never delayed. `force`
	/// overrides the debounce, for end of stream.
	pub fn flush(&mut self, pts: Timestamp, force: bool) -> anyhow::Result<()> {
		let now = kio::clock::now();
		for (key, entry) in self.entries.iter_mut() {
			entry.expire(self.now);
			if !entry.dirty {
				continue;
			}
			if !force
				&& let Some(last) = entry.last_cut
				&& now.saturating_duration_since(last) < DEBOUNCE
			{
				continue;
			}
			entry.cut(&self.broadcast, *key, pts, now)?;
		}

		if self.entries.values().any(|e| e.advertised != e.populated) {
			let mut guard = self.catalog.modify()?;
			let Some(mpegts) = guard.ext.mpegts_mut() else {
				anyhow::bail!("catalog extension no longer carries an mpegts section");
			};
			for ((pid, table_id), entry) in self.entries.iter_mut() {
				// Either flag implies the track exists.
				let Some(track) = &entry.track else { continue };
				if entry.advertised == entry.populated {
					continue;
				}
				if entry.populated {
					mpegts.si.entry(*pid).or_default().insert(
						*table_id,
						catalog::SiEntry {
							track: track.name().to_string(),
							interval: entry.interval,
							..Default::default()
						},
					);
				} else {
					retract(&mut mpegts.si, *pid, *table_id, track.name());
				}
				entry.advertised = entry.populated;
			}
		}
		Ok(())
	}

	/// Flush everything and finish every track.
	pub fn finish(&mut self, pts: Timestamp) -> anyhow::Result<()> {
		self.flush(pts, true)?;
		for track in self.entries.values_mut().filter_map(|entry| entry.track.as_mut()) {
			track.finish()?;
		}
		Ok(())
	}

	/// Abort every track with `err` instead of finishing, removing their catalog
	/// entries so the map never names an aborted track.
	pub fn abort(mut self, err: moq_net::Error) {
		self.unadvertise();
		for (_, entry) in std::mem::take(&mut self.entries) {
			if let Some(track) = entry.track {
				let _ = track.abort(err.clone());
			}
		}
	}

	/// Remove every advertised entry from the catalog's `si` map, once (idempotent:
	/// entries are marked unadvertised as they go, so the `Drop` after an `abort`
	/// finds nothing left to do).
	fn unadvertise(&mut self) {
		if !self.entries.values().any(|e| e.advertised) {
			return;
		}
		// A closed catalog has nothing left to unadvertise from.
		let Ok(mut guard) = self.catalog.modify() else {
			return;
		};
		let Some(mpegts) = guard.ext.mpegts_mut() else {
			return;
		};
		for ((pid, table_id), entry) in self.entries.iter_mut() {
			let Some(track) = entry.track.as_ref().filter(|_| entry.advertised) else {
				continue;
			};
			entry.advertised = false;
			retract(&mut mpegts.si, *pid, *table_id, track.name());
		}
	}
}

/// Remove the `(pid, table_id)` mapping from the catalog's `si` map, and the PID
/// once nothing is left on it.
///
/// Only a mapping still naming `track` goes: a second capture on the same broadcast
/// (same key, unique fallback track name) may have overwritten it, and it will not
/// re-advertise, so blindly removing here would strip the live capture's table for
/// good.
fn retract(si: &mut BTreeMap<u16, BTreeMap<u8, catalog::SiEntry>>, pid: u16, table_id: u8, track: &str) {
	let Some(tables) = si.get_mut(&pid) else {
		return;
	};
	if tables.get(&table_id).is_some_and(|e| e.track == track) {
		tables.remove(&table_id);
	}
	if tables.is_empty() {
		si.remove(&pid);
	}
}

impl<E: catalog::Catalog> Drop for Capture<E> {
	fn drop(&mut self) {
		self.unadvertise();
	}
}

/// Export-side reduction of a snapshot track: frames apply in order, later-wins
/// by sub-table identity. Reset per group (a group is a complete snapshot).
/// Equality is by content, so a re-published identical snapshot is not a change.
#[derive(Default, PartialEq)]
pub(super) struct Snapshot {
	subs: BTreeMap<Identity, Vec<Bytes>>,
}

impl Snapshot {
	/// Apply one frame: the payload's sections replace the sub-table they identify.
	pub fn apply(&mut self, payload: &Bytes) {
		let sections = split_sections(payload);
		let Some(first) = sections.first() else {
			return;
		};
		self.subs.insert(identity(first), sections);
	}

	/// Every section in the snapshot, in stable sub-table order.
	pub fn sections(&self) -> impl Iterator<Item = &Bytes> {
		self.subs.values().flatten()
	}

	pub fn is_empty(&self) -> bool {
		self.subs.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::container::ts::fixture::{Flags, Mux};

	#[kio::test]
	async fn program_clock_places_and_retires_si_without_media_pts() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<catalog::Ext>::default()),
		)
		.unwrap();
		let mut import = super::super::Import::new(broadcast, catalog.reserve());
		let mut mux = Mux::default();
		mux.pat(&[(1, 0x20)]).pmt(0x20, 1, 0x1ff, &[], 0);
		let sdt = psi::section(0x42, 1, 0, 0, 0, &[0, 1, 0xff]);
		// Before the first PCR, and past it with no interval to extrapolate from, the program
		// clock cannot age SI, so it is dropped as an unplaced whole PES is.
		mux.section(0x11, &sdt);
		mux.packet(0x1ff, Flags::pcr(0));
		mux.section(0x11, &sdt);
		import.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert!(catalog.snapshot().ext.mpegts.si.is_empty());
		let row = &import.stats().streams[&0x11];
		assert_eq!((row.track.as_str(), row.unplaced), (".si", 2));
		mux.packet(0x1ff, Flags::pcr(40 * 27_000));
		mux.section(0x11, &sdt);
		import.decode(&std::mem::take(&mut mux.out)).unwrap();
		assert!(catalog.snapshot().ext.mpegts.si.contains_key(&0x11));
		assert_eq!(import.stats().streams[&0x11].unplaced, 2);
		// Accepted PCR intervals move the program clock; no video PTS or host time passes.
		for n in 2..=52 {
			mux.packet(0x1ff, Flags::pcr(n * 40 * 27_000));
			import.decode(&std::mem::take(&mut mux.out)).unwrap();
		}
		assert!(catalog.snapshot().ext.mpegts.si.is_empty());
	}
	fn capture() -> (Capture<catalog::Ext>, crate::catalog::Producer<catalog::Ext>) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<catalog::Ext>::default()),
		)
		.unwrap();
		(Capture::new(broadcast, catalog.clone()), catalog)
	}

	fn section(table: u8, id: u16, version: u8, last: u8) -> Bytes {
		psi::section(table, id, version, 0, last, &[0, 0, 0, 0, 0, 0]).into()
	}

	const MS: u64 = 27_000;

	/// Drives a capture's program clock from test time zero: a PCR every 40 ms, ten input
	/// packets apart, each section completed by the packet carrying the latest PCR.
	struct Input {
		/// Ticks past the first PCR, which every interval so far accepted.
		pcr: u64,
		count: u64,
	}

	impl Input {
		/// One valid interval measures the rate, so SI is placed from test time zero.
		fn new(capture: &mut Capture<catalog::Ext>) -> Self {
			capture.pcr(0, 0);
			let mut input = Self { pcr: 0, count: 0 };
			input.step(capture, 40 * MS);
			input
		}

		fn step(&mut self, capture: &mut Capture<catalog::Ext>, ticks: u64) {
			self.pcr += ticks;
			self.count += 10;
			capture.pcr(self.pcr % PCR_WRAP, self.count);
		}

		/// Advance the program clock to `t` past test time zero, rounding up to a whole tick.
		fn to(&mut self, capture: &mut Capture<catalog::Ext>, t: Duration) {
			let target = 40 * MS + u64::try_from((t.as_nanos() * 27).div_ceil(1_000)).unwrap();
			while self.pcr < target {
				self.step(capture, (target - self.pcr).min(40 * MS));
			}
		}

		fn section(&self, capture: &mut Capture<catalog::Ext>, pid: u16, section: Bytes) {
			assert!(capture.section(pid, section, self.count), "placed");
		}
	}

	#[kio::test]
	async fn incomplete_generations_expire_on_their_table_intervals() {
		let (mut capture, _) = capture();
		let mut input = Input::new(&mut capture);
		for id in 0..4096 {
			for table in [0x4e, 0x80] {
				input.section(&mut capture, 0x12, section(table, id, 0, 1));
			}
		}
		let payload = capture.entries[&(0x12, 0x4e)].pending.values().next().unwrap().sections[&0].clone();
		assert!(!payload.is_unique());
		input.to(&mut capture, Duration::from_secs(2));
		capture.flush(Timestamp::ZERO, false).unwrap();
		assert_eq!(
			capture.entries[&(0x12, 0x4e)].pending.len(),
			4096,
			"the deadline is inclusive"
		);
		input.to(&mut capture, Duration::from_secs(2) + Duration::from_nanos(1));
		capture.flush(Timestamp::ZERO, false).unwrap();
		let eit = &capture.entries[&(0x12, 0x4e)];
		assert!(eit.pending.is_empty());
		assert!(eit.expiry.is_empty());
		assert!(
			payload.is_unique(),
			"expiry releases the generation's payload ownership"
		);
		assert_eq!(capture.entries[&(0x12, 0x80)].pending.len(), 4096);
		input.to(&mut capture, Duration::from_secs(30) + Duration::from_nanos(1));
		capture.flush(Timestamp::ZERO, false).unwrap();
		assert!(
			capture
				.entries
				.values()
				.all(|e| e.pending.is_empty() && e.expiry.is_empty())
		);
	}

	#[kio::test]
	async fn pending_renewal_replaces_its_deadline_without_renewing_other_versions() {
		let (mut capture, _) = capture();
		let mut input = Input::new(&mut capture);
		capture.select(1);
		for version in [0, 1] {
			input.section(&mut capture, 0x11, section(0x42, 1, version, 2));
		}
		input.to(&mut capture, Duration::from_millis(1500));
		for _ in 0..1000 {
			input.section(&mut capture, 0x11, section(0x42, 1, 0, 2));
		}
		assert_eq!(capture.entries[&(0x11, 0x42)].expiry.len(), 2);
		input.to(&mut capture, Duration::from_millis(2100));
		capture.flush(Timestamp::ZERO, false).unwrap();
		let entry = &capture.entries[&(0x11, 0x42)];
		assert_eq!(entry.pending.len(), 1);
		assert!(entry.pending.contains_key(&(Identity::Sub { ext: 1, scope: 0 }, 0)));
		assert_eq!(entry.expiry.len(), 1);
		input.to(&mut capture, Duration::from_millis(3501));
		capture.flush(Timestamp::ZERO, false).unwrap();
		assert!(capture.entries[&(0x11, 0x42)].pending.is_empty());
	}

	#[kio::test]
	async fn retirement_publishes_reduced_and_empty_snapshots_without_host_debounce() {
		let (mut capture, catalog) = capture();
		let mut input = Input::new(&mut capture);
		for id in [1, 2] {
			input.section(&mut capture, 0x10, section(0x40, id, 0, 0));
		}
		capture.flush(Timestamp::ZERO, false).unwrap();
		let name = catalog.snapshot().ext.mpegts.si[&0x10][&0x40].track.clone();
		let mut reader = capture
			.broadcast
			.consume()
			.track(&name)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let mut original = reader.recv_group().await.unwrap().unwrap();
		input.to(&mut capture, Duration::from_secs(9));
		for _ in 0..1000 {
			input.section(&mut capture, 0x10, section(0x40, 1, 0, 0));
		}
		assert_eq!(capture.entries[&(0x10, 0x40)].expiry.len(), 2);
		input.to(&mut capture, Duration::from_millis(10_001));
		capture.flush(Timestamp::ZERO, false).unwrap();
		let mut reduced = reader.recv_group().await.unwrap().unwrap();
		let frame = reduced.read_frame().await.unwrap().unwrap();
		assert_eq!(identity(&frame.payload), Identity::Sub { ext: 1, scope: 0 });
		assert!(reduced.read_frame().await.unwrap().is_none());
		assert_eq!(catalog.snapshot().ext.mpegts.si[&0x10][&0x40].track, name);
		input.to(&mut capture, Duration::from_millis(19_001));
		capture.flush(Timestamp::ZERO, false).unwrap();
		let mut empty = reader.recv_group().await.unwrap().unwrap();
		assert!(empty.read_frame().await.unwrap().is_none());
		assert!(catalog.snapshot().ext.mpegts.si.is_empty());
		assert!(capture.entries[&(0x10, 0x40)].active.is_empty());
		assert!(capture.entries[&(0x10, 0x40)].expiry.is_empty());
		// Retraction cannot invalidate a reader already holding a prior snapshot.
		assert!(original.read_frame().await.unwrap().is_some());
		input.section(&mut capture, 0x10, section(0x40, 1, 0, 0));
		capture.flush(Timestamp::ZERO, true).unwrap();
		let mut resumed = reader.recv_group().await.unwrap().unwrap();
		assert!(resumed.read_frame().await.unwrap().is_some());
		assert_eq!(catalog.snapshot().ext.mpegts.si[&0x10][&0x40].track, name);
	}

	#[kio::test]
	async fn host_time_and_new_si_at_one_input_position_do_not_age_si() {
		let (mut capture, _) = capture();
		let input = Input::new(&mut capture);
		input.section(&mut capture, 0x12, section(0x4e, 0, 0, 0));
		capture.flush(Timestamp::ZERO, false).unwrap();
		kio::clock::sleep(Duration::from_secs(60)).await;
		for id in 1..1024 {
			input.section(&mut capture, 0x12, section(0x4e, id, 0, 1));
		}
		capture.flush(Timestamp::ZERO, false).unwrap();
		let entry = &capture.entries[&(0x12, 0x4e)];
		assert_eq!(entry.active.len(), 1);
		assert_eq!(entry.pending.len(), 1023);
		assert_eq!(entry.expiry.len(), 1024);
	}

	#[test]
	fn the_clock_counts_valid_intervals_and_extrapolates_the_rest() {
		let mut clock = Clock::default();
		let at = |clock: &Clock, count| clock.at(count).map(|t| t.as_millis());
		clock.pcr(PCR_WRAP - 40 * MS, 0);
		assert_eq!(at(&clock, 0), None, "no interval measures the rate");
		// Across the wrap: 40 ms over 10 packets.
		clock.pcr(0, 10);
		assert_eq!(at(&clock, 10), Some(40));
		// A stall, a rewind, a corrupt jump and a flagged break each charge the input they
		// span at the measured rate, not their PCR values.
		clock.pcr(0, 20);
		clock.pcr(PCR_WRAP - 1_000 * MS, 30);
		clock.pcr(100_000 * MS, 40);
		clock.discontinuity();
		clock.pcr(40 * MS, 50);
		assert_eq!(at(&clock, 50), Some(200));
		// The interval still open is charged only once it outruns any valid interval.
		assert_eq!(at(&clock, 50 + 250), Some(200));
		assert_eq!(at(&clock, 50 + 260), Some(240));
		clock.pcr(80 * MS, 60);
		assert_eq!(at(&clock, 60), Some(240));
		// Ten seconds of PCR silence, then a valid step: the silence stays charged and the
		// clock runs on from there rather than freezing until the PCR catches up.
		assert_eq!(at(&clock, 60 + 2_500), Some(9_240));
		clock.pcr(120 * MS, 2_560);
		assert_eq!(at(&clock, 2_560), Some(9_240));
		clock.pcr(160 * MS, 2_570);
		assert_eq!(at(&clock, 2_570), Some(9_280));
	}

	#[test]
	fn the_clock_follows_a_rate_increase() {
		let mut clock = Clock::default();
		let at = |clock: &Clock, count| clock.at(count).map(|t| t.as_millis());
		for n in 0..=10 {
			clock.pcr(n * 40 * MS, n * 10);
		}
		let mut count = 100;
		// 26 times the packets per interval: each valid step still counts as its own 40 ms,
		// rather than the second its packets span at the old rate.
		for n in 11..=100 {
			count += 260;
			clock.pcr(n * 40 * MS, count);
		}
		assert_eq!(at(&clock, count), Some(4_000));
	}

	#[kio::test]
	async fn fresh_si_ages_out_while_the_pcr_stalls() {
		for silent in [false, true] {
			let (mut capture, _) = capture();
			let mut input = Input::new(&mut capture);
			let stalled = input.pcr;
			let mut peak = 0;
			for id in 0..2000 {
				input.count += 10;
				if !silent {
					capture.pcr(stalled, input.count);
				}
				input.section(&mut capture, 0x12, section(0x4e, id, 0, 1));
				capture.flush(Timestamp::ZERO, false).unwrap();
				let entry = &capture.entries[&(0x12, 0x4e)];
				assert_eq!(entry.expiry.len(), entry.pending.len());
				peak = peak.max(entry.pending.len());
			}
			// Each identity arrives 40 ms of input after the last and lives the table's 2 s. A
			// silent clock charges nothing for the open interval's first second, so the 25
			// identities in it outlive their 2 s once.
			assert_eq!(peak, if silent { 75 } else { 51 }, "silent: {silent}");
			assert_eq!(capture.entries[&(0x12, 0x4e)].pending.len(), 51, "silent: {silent}");
		}
	}

	#[kio::test]
	async fn short_form_repetitions_replace_their_deadline_and_latest_value() {
		let (mut capture, _) = capture();
		let mut input = Input::new(&mut capture);
		let mut time = vec![0x70, 0x70, 5, 0, 0, 0, 0, 0];
		input.section(&mut capture, 0x14, Bytes::from(time.clone()));
		input.to(&mut capture, Duration::from_secs(29));
		input.section(&mut capture, 0x14, Bytes::from(time.clone()));
		input.to(&mut capture, Duration::from_secs(31));
		time[7] = 1;
		input.section(&mut capture, 0x14, Bytes::from(time.clone()));
		input.to(&mut capture, Duration::from_secs(60));
		capture.flush(Timestamp::ZERO, false).unwrap();
		let entry = &capture.entries[&(0x14, 0x70)];
		assert_eq!(entry.expiry.len(), 1);
		assert_eq!(entry.active[&Identity::Table].sections[&0], Bytes::from(time));
		input.to(&mut capture, Duration::from_millis(61_001));
		capture.flush(Timestamp::ZERO, false).unwrap();
		assert!(capture.entries[&(0x14, 0x70)].active.is_empty());
		assert!(capture.entries[&(0x14, 0x70)].expiry.is_empty());
	}
}
