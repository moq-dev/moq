use std::fmt;

/// A lite protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Lite01,
	Lite02,
	Lite03,
	Lite04,
	/// lite-05. Adds the TRACK stream (immutable per-track properties incl.
	/// timescale), zigzag-delta timestamps in per-frame headers, and drops
	/// SUBSCRIBE_OK/FETCH_OK.
	Lite05,
	/// Lite-06. Adds announce ids: each `active` ANNOUNCE_BROADCAST
	/// implicitly assigns the next ordinal, and `ended`/`restart` reference that id
	/// instead of repeating the path. Also adds the route cost carried alongside the
	/// hop chain, ranking above hop count in route selection. Advertised over ALPN
	/// as `moq-lite-06` and preferred by the default version sets.
	Lite06,
	/// Lite-07. Adds the hidden opt-in to ANNOUNCE_REQUEST: without it, a route with
	/// a `.`-prefixed segment below the requested prefix is left out. SUBSCRIBE_DROP
	/// accounts for every group the subscription will not deliver.
	/// ANNOUNCE_START and ANNOUNCE_UPDATE may copy a path head or hop-chain tail from a
	/// live announcement on the same stream. Adds the Auth Stream. SUBSCRIBE carries a
	/// list of ranges. The wire format is
	/// still work-in-progress, so it is advertised over ALPN as `moq-lite-07-wip` and
	/// only when explicitly requested; the default version sets leave it out.
	Lite07,
}

// Feature predicates list the old versions explicitly, so a future version falls
// forward to the newest behavior.
impl Version {
	/// Whether SETUP recognizes the Implementation parameter.
	pub(crate) fn has_implementation(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether SUBSCRIBE_OK carries the publisher's largest (group, frame), which a
	/// subscriber takes as where the live feed is. Added in lite-07; an earlier answer
	/// says nothing about the live edge.
	pub(crate) fn has_largest(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether a copy received over this version resolves relative and time range anchors
	/// as its publisher would: the start resolves from the budget, the answer reports the
	/// largest, and frames carry media time. Added in lite-07; an earlier copy refuses them.
	pub(crate) fn resolves_anchors(self) -> bool {
		self.resolves_start() && self.has_largest() && self.has_media_time()
	}

	/// Whether the version has lite-05's dedicated TRACK stream and related stream
	/// layout changes.
	///
	/// This is the common feature boundary for TRACK_INFO, FETCH streams,
	/// SUBSCRIBE_START/END, and per-frame timestamp prefixes.
	pub fn has_track_stream(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04)
	}

	/// Whether the PROBE message carries the RTT field. Added in lite-04; lite-03
	/// carries the bitrate alone, so a report with only an RTT to give says nothing
	/// there and must not be sent.
	pub fn has_probe_rtt(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03)
	}

	/// Whether the session opens a unidirectional Setup Stream carrying a single SETUP
	/// message (capabilities + optional Path). Added in lite-05; the older bidirectional
	/// setup exchange (Lite01/02) and the no-setup drafts (Lite03/04) don't use it.
	pub fn has_setup_stream(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04)
	}

	/// Whether the session may deliver groups over unreliable QUIC datagrams (lite-05 §6.4).
	/// A datagram carries one single-frame group's `subscribe | sequence | timestamp | payload`
	/// and is routed over the existing subscription. Added in lite-05; older versions never
	/// send or accept datagram bodies.
	pub fn has_datagrams(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04)
	}

	/// Whether announce streams begin with ANNOUNCE_OK and omit the sender's origin
	/// from each announcement's hop chain. Added in lite-05.
	pub fn has_announce_ok(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04)
	}

	/// Whether the session supports the GOAWAY control stream (0x5) for graceful
	/// shutdown and migration. Added in lite-04.
	pub fn has_goaway(self) -> bool {
		!matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03)
	}

	/// Whether either endpoint may open an Auth Stream (0x7) to present a token and
	/// learn its grant. Added in lite-07.
	pub fn has_auth(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether either endpoint may open a Padding Stream (0x2). Added in lite-07.
	pub(crate) fn has_padding(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether a subscriber may open a Tracks Stream (0x8) to list a broadcast's
	/// tracks. Added in lite-07.
	pub fn has_tracks(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether announcements carry implicit announce ids: each `active`
	/// ANNOUNCE_BROADCAST assigns the next per-stream ordinal, and `ended`/`restart`
	/// reference that id instead of repeating the path. Added in lite-06.
	pub fn has_announce_id(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05
		)
	}

	/// Whether ANNOUNCE_REQUEST carries the Exclude Hop field: the subscriber's own
	/// Hop ID, which the publisher uses to skip announces whose hop chain already
	/// passed through the subscriber. Present in lite-04 and lite-05 only.
	///
	/// The receiver's own reflected-announce check drops those announces anyway (and
	/// catches loops of any length, not just the two-hop case), so lite-06 drops the
	/// field and keeps the check. Lite-06 also declares the same identity session-wide
	/// in the SETUP `Hop` parameter, which filters announcements and subscriptions
	/// alike rather than one announce stream.
	///
	/// Unlike the gates above, this lists the versions that *have* the field: it was
	/// removed rather than added, so future versions default to not carrying it.
	pub fn has_exclude_hop(self) -> bool {
		matches!(self, Self::Lite04 | Self::Lite05)
	}

	/// Whether SUBSCRIBE, SUBSCRIBE_UPDATE, FETCH, and GROUP carry frame indices
	/// alongside their group sequences, so a subscription or fetch can start and end
	/// partway through a group. Added in lite-06.
	///
	/// Older versions only address whole groups, so a route change has to wait for the
	/// next group before it can resume.
	pub fn has_frame_bounds(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05
		)
	}

	/// Whether SUBSCRIBE and SUBSCRIBE_UPDATE carry the subscriber's `Group Order`.
	/// Added in lite-07; an earlier subscription is served newest first.
	pub(crate) fn has_order(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether SUBSCRIBE and SUBSCRIBE_UPDATE carry a list of ranges, each end absolute,
	/// relative to the latest group, or at a presentation time. Added in lite-07; an earlier
	/// subscription carries one span with absolute ends.
	pub(crate) fn has_ranges(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether FETCH carries the subscriber's optional max delay. Added in lite-07; an
	/// earlier fetch is bounded by the publisher's retention alone.
	pub(crate) fn has_fetch_max_delay(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether this version's SUBSCRIBE, SUBSCRIBE_UPDATE, SUBSCRIBE_OK, and TRACK_INFO
	/// carry the retired `Ordered` byte.
	///
	/// These drafts are served newest first whatever the byte says, so it is written as 0
	/// and ignored on read rather than shifting every field behind it. Lite-07's
	/// `Group Order` (see [`Self::has_order`]) is a new field, not this one.
	pub(crate) fn has_group_order(self) -> bool {
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 => true,
			Self::Lite06 | Self::Lite07 => false,
		}
	}

	/// Whether SUBSCRIBE's `Group Start` is an absolute floor the publisher resolves a
	/// start from: the raw minimum group sequence (default 0), with `Subscriber Max Age`
	/// as the only gate on how far back delivery begins. Changed in lite-06.
	///
	/// Older versions encode `Group Start` as the sequence + 1, with 0 meaning the
	/// latest group, so an absent start there pins the cursor to the live edge instead
	/// of resolving it from the budget.
	pub(crate) fn resolves_start(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05
		)
	}

	/// Whether a copy's fills stop at the start its publisher resolved from the budget.
	/// Lite-06 resolves the start, but drops media time and sends FETCH without a max
	/// delay, so nothing judges a group below that start against a reader's budget.
	pub(crate) fn fills_from_start(self) -> bool {
		self.resolves_start() && !self.has_media_time() && !self.has_fetch_max_delay()
	}

	/// Whether subscription completion waits for the subscriber's FIN.
	pub(crate) fn waits_for_subscriber_fin(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether ANNOUNCE_REQUEST carries the hidden opt-in. Added in lite-07; older
	/// requests decode as not opted in.
	pub fn has_hidden(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether ANNOUNCE_START and ANNOUNCE_UPDATE may copy a path head or hop-chain tail
	/// from a live announcement on the same stream. Added in lite-07.
	pub fn has_announce_compression(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether TRACK_INFO carries the compression flag, and a compressed Track's FRAME
	/// and datagram its inflated size. Added in lite-07; an earlier peer receives the
	/// frames inflated.
	pub(crate) fn has_compression(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether the stream error table has NOT_FETCHABLE, for a FETCH that reached a
	/// datagram. Added in lite-07; earlier versions say NOT_FOUND instead.
	pub(crate) fn has_not_fetchable(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether SUBSCRIBE_DROP's Error Code is a stream error code, ending a dropped group's
	/// reader as a reset keeping its header would. Added in lite-07; earlier versions carry
	/// an application-specific code there.
	#[cfg(feature = "async")]
	pub(crate) fn has_drop_code(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether announcements and track requests carry the publisher epoch, the
	/// identity that says two routes serve the same bytes. Added in lite-07.
	/// Older versions carry nothing, so a received route has no epoch and is
	/// never resumed through another route.
	pub fn has_epoch(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether a session may negotiate the `ietf` extension (draft-lcurley-moq-ietf), which
	/// carries moq-transport object state between relays. Added on lite-07; published
	/// versions get no framing changes.
	pub(crate) fn has_ietf(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether the announce stream has ANNOUNCE_RESTART: another publisher instance
	/// replacing an advertisement in place. Added in lite-07. Older versions send an
	/// ANNOUNCE_END then an ANNOUNCE_START.
	pub fn has_announce_restart(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether TRACK_INFO carries the refresh ratio and SUBSCRIBE_UPDATE a wanted group,
	/// so a join at any hop reaches the publisher. Added in lite-07; older peers see a
	/// track that never refreshes.
	pub(crate) fn has_refresh(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether received frame timestamps are the publisher's media time. Lite05/06 stamp an
	/// untimed track's payloads with their send time, which a receiver cannot tell from it,
	/// so it drops every timestamp received on them and the track arrives untimed.
	pub(crate) fn has_media_time(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether discovery streams end their initial set with SYNCED rather than a count.
	pub fn has_synced(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06
		)
	}

	/// Whether announcements carry the route cost: the marginal cost of pulling
	/// the broadcast via this route, accumulated per link. Added in lite-06.
	/// Older versions carry nothing, so a received route stays at zero and ranks
	/// on hop count alone, exactly as before.
	pub fn has_route_cost(self) -> bool {
		!matches!(
			self,
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05
		)
	}

	/// The timescale the wire carries for a track with `timescale`, or `None` when its
	/// frames and datagrams carry no timestamp.
	///
	/// Lite05 and Lite06 always carry one, so an untimed track declares milliseconds there
	/// and its payloads carry their send times instead.
	pub(crate) fn wire_timescale(self, timescale: Option<crate::Timescale>) -> Option<crate::Timescale> {
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => None,
			Self::Lite05 | Self::Lite06 => Some(timescale.unwrap_or(crate::Timescale::MILLI)),
			_ => timescale,
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Lite01 => write!(f, "moq-lite-01"),
			Self::Lite02 => write!(f, "moq-lite-02"),
			Self::Lite03 => write!(f, "moq-lite-03"),
			Self::Lite04 => write!(f, "moq-lite-04"),
			Self::Lite05 => write!(f, "moq-lite-05"),
			Self::Lite06 => write!(f, "moq-lite-06"),
			Self::Lite07 => write!(f, "moq-lite-07-wip"),
		}
	}
}

impl From<Version> for crate::Version {
	fn from(v: Version) -> Self {
		match v {
			Version::Lite01 => crate::Version::Lite(Version::Lite01),
			Version::Lite02 => crate::Version::Lite(Version::Lite02),
			Version::Lite03 => crate::Version::Lite(Version::Lite03),
			Version::Lite04 => crate::Version::Lite(Version::Lite04),
			Version::Lite05 => crate::Version::Lite(Version::Lite05),
			Version::Lite06 => crate::Version::Lite(Version::Lite06),
			Version::Lite07 => crate::Version::Lite(Version::Lite07),
		}
	}
}

impl TryFrom<crate::Version> for Version {
	type Error = ();

	fn try_from(v: crate::Version) -> Result<Self, Self::Error> {
		match v {
			crate::Version::Lite(v) => Ok(v),
			crate::Version::Ietf(_) => Err(()),
		}
	}
}
