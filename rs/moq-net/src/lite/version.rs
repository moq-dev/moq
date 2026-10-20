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
	/// live announcement on the same stream. Adds the Auth Stream. The wire format is
	/// still work-in-progress, so it is advertised over ALPN as `moq-lite-07-wip` and
	/// only when explicitly requested; the default version sets leave it out.
	Lite07,
}

impl Version {
	/// Whether SUBSCRIBE_OK carries the publisher's largest (group, frame), which a
	/// subscriber takes as where the live feed is. Added in lite-07; an earlier answer
	/// says nothing about the live edge.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_largest(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether the version has lite-05's dedicated TRACK stream and related stream
	/// layout changes.
	///
	/// This is the common feature boundary for TRACK_INFO, FETCH streams,
	/// SUBSCRIBE_START/END, and per-frame timestamp prefixes.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_track_stream(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether the PROBE message carries the RTT field. Added in lite-04; lite-03
	/// carries the bitrate alone, so a report with only an RTT to give says nothing
	/// there and must not be sent.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_probe_rtt(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 => false,
			_ => true,
		}
	}

	/// Whether the session opens a unidirectional Setup Stream carrying a single SETUP
	/// message (capabilities + optional Path). Added in lite-05; the older bidirectional
	/// setup exchange (Lite01/02) and the no-setup drafts (Lite03/04) don't use it.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_setup_stream(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether the session may deliver groups over unreliable QUIC datagrams (lite-05 §6.4).
	/// A datagram carries one single-frame group's `subscribe | sequence | timestamp | payload`
	/// and is routed over the existing subscription. Added in lite-05; older versions never
	/// send or accept datagram bodies.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_datagrams(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether announce streams begin with ANNOUNCE_OK and omit the sender's origin
	/// from each announcement's hop chain. Added in lite-05.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_announce_ok(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether the session supports the GOAWAY control stream (0x5) for graceful
	/// shutdown and migration. Added in lite-04.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_goaway(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 => false,
			_ => true,
		}
	}

	/// Whether either endpoint may open an Auth Stream (0x7) to present a token and
	/// learn its grant. Added in lite-07.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_auth(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether announcements carry implicit announce ids: each `active`
	/// ANNOUNCE_BROADCAST assigns the next per-stream ordinal, and `ended`/`restart`
	/// reference that id instead of repeating the path. Added in lite-06.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_announce_id(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 => false,
			_ => true,
		}
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
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_frame_bounds(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 => false,
			_ => true,
		}
	}

	/// Whether SUBSCRIBE and SUBSCRIBE_UPDATE carry the subscriber's `Group Order`.
	/// Added in lite-07; an earlier subscription is served newest first.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_order(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether FETCH carries the subscriber's optional max delay. Added in lite-07; an
	/// earlier fetch is bounded by the publisher's retention alone.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_fetch_max_delay(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
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
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn resolves_start(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 => false,
			_ => true,
		}
	}

	/// Whether subscription completion waits for the subscriber's FIN.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn waits_for_subscriber_fin(self) -> bool {
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether ANNOUNCE_REQUEST carries the hidden opt-in. Added in lite-07; older
	/// requests decode as not opted in.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_hidden(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether ANNOUNCE_START and ANNOUNCE_UPDATE may copy a path head or hop-chain tail
	/// from a live announcement on the same stream. Added in lite-07.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_announce_compression(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether the stream error table has NOT_FETCHABLE, for a FETCH that reached a
	/// datagram. Added in lite-07; earlier versions say NOT_FOUND instead.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_not_fetchable(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether announcements and track requests carry the publisher epoch, the
	/// identity that says two routes serve the same bytes. Added in lite-07.
	/// Older versions carry nothing, so a received route has no epoch and is
	/// never resumed through another route.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_epoch(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether a session may negotiate the `ietf` extension (draft-lcurley-moq-ietf), which
	/// carries moq-transport object state between relays. Added on lite-07; published
	/// versions get no framing changes.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_ietf(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether the announce stream has ANNOUNCE_RESTART: another publisher instance
	/// replacing an advertisement in place. Added in lite-07. Older versions send an
	/// ANNOUNCE_END then an ANNOUNCE_START.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_announce_restart(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether TRACK_INFO carries the refresh ratio and SUBSCRIBE_UPDATE a wanted group,
	/// so a join at any hop reaches the publisher. Added in lite-07; older peers see a
	/// track that never refreshes.
	#[allow(clippy::match_like_matches_macro)]
	pub(crate) fn has_refresh(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether discovery streams end their initial set with SYNCED rather than a count.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_synced(self) -> bool {
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 | Self::Lite06 => false,
			_ => true,
		}
	}

	/// Whether announcements carry the route cost: the marginal cost of pulling
	/// the broadcast via this route, accumulated per link. Added in lite-06.
	/// Older versions carry nothing, so a received route stays at zero and ranks
	/// on hop count alone, exactly as before.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_route_cost(self) -> bool {
		// Match form so future versions default forward (AGENTS.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 | Self::Lite05 => false,
			_ => true,
		}
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
