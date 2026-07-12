export const Version = {
	DRAFT_01: 0xff0dad01,
	DRAFT_02: 0xff0dad02,
	DRAFT_03: 0xff0dad03,
	DRAFT_04: 0xff0dad04,
	DRAFT_05: 0xff0dad05,
	/// Work-in-progress lite-06, advertised as the preferred WebTransport subprotocol.
	/// Adds announce ids: each active ANNOUNCE_BROADCAST implicitly assigns the next
	/// ordinal, and ended/restart reference that id instead of repeating the path.
	DRAFT_06: 0xff0dad06,
} as const;

export type Version = (typeof Version)[keyof typeof Version];

/// Whether the session opens a unidirectional Setup Stream carrying a single SETUP message
/// (capabilities + optional Path). Added in lite-05; older drafts have no Setup Stream.
export function hasSetupStream(version: Version): boolean {
	// Explicitly list older versions so future versions default to having the stream.
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			return false;
		default:
			return true;
	}
}

/// Whether the session may deliver groups over unreliable QUIC datagrams (lite-05 §6.4).
/// A datagram carries one single-frame group's `subscribe | sequence | timestamp | payload` and is
/// routed over the existing subscription. Added in lite-05; older versions never send/accept them.
export function hasDatagrams(version: Version): boolean {
	// Explicitly list older versions so future versions default to having datagrams.
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			return false;
		default:
			return true;
	}
}

/** Whether announce streams begin with ANNOUNCE_OK and omit the sender's origin from each hop chain. */
export function hasAnnounceOk(version: Version): boolean {
	// Explicitly list older versions so future versions keep the lite-05+ announce behavior.
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			return false;
		default:
			return true;
	}
}

/** Whether announcements carry implicit announce ids: each `active` assigns the next
 * per-stream ordinal, and `ended`/`restart` reference that id instead of repeating the
 * path. Added in lite-06. */
export function hasAnnounceId(version: Version): boolean {
	// Explicitly list older versions so future versions keep the lite-06+ announce behavior.
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
		case Version.DRAFT_05:
			return false;
		default:
			return true;
	}
}

/// The WebTransport subprotocol identifier for moq-lite.
/// Version negotiation still happens via SETUP when this is used.
export const ALPN = "moql";

/// The ALPN string for Draft03, which uses ALPN-based version negotiation.
export const ALPN_03 = "moq-lite-03";

/// The ALPN string for Draft04, which uses ALPN-based version negotiation.
export const ALPN_04 = "moq-lite-04";

/// The ALPN string for Draft05, which uses ALPN-based version negotiation.
export const ALPN_05 = "moq-lite-05";

/// The ALPN string for the work-in-progress Draft06. It is NOT in the default
/// WebTransport `protocols` list, so lite-06 is never advertised or negotiated by
/// default; a peer only reaches it when both sides explicitly offer this ALPN.
export const ALPN_06_WIP = "moq-lite-06-wip";

const VERSION_NAMES: Record<number, string> = {
	[Version.DRAFT_01]: "moq-lite-01",
	[Version.DRAFT_02]: "moq-lite-02",
	[Version.DRAFT_03]: "moq-lite-03",
	[Version.DRAFT_04]: "moq-lite-04",
	[Version.DRAFT_05]: "moq-lite-05",
	[Version.DRAFT_06]: "moq-lite-06-wip",
};

export function versionName(v: Version): string {
	return VERSION_NAMES[v] ?? `unknown(0x${v.toString(16)})`;
}
