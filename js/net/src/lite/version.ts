export const Version = {
	DRAFT_01: 0xff0dad01,
	DRAFT_02: 0xff0dad02,
	DRAFT_03: 0xff0dad03,
	DRAFT_04: 0xff0dad04,
	/// Work-in-progress lite-05, advertised as the preferred WebTransport
	/// subprotocol so the demo and tests negotiate it. Still WIP; revisit before
	/// promoting to `main`.
	DRAFT_05_WIP: 0xff0dad05,
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

/// The WebTransport subprotocol identifier for moq-lite.
/// Version negotiation still happens via SETUP when this is used.
export const ALPN = "moql";

/// The ALPN string for Draft03, which uses ALPN-based version negotiation.
export const ALPN_03 = "moq-lite-03";

/// The ALPN string for Draft04, which uses ALPN-based version negotiation.
export const ALPN_04 = "moq-lite-04";

/// The ALPN string for the work-in-progress Draft05, offered first in the
/// default WebTransport `protocols` list so lite-05 is the preferred version.
export const ALPN_05_WIP = "moq-lite-05-wip";

const VERSION_NAMES: Record<number, string> = {
	[Version.DRAFT_01]: "moq-lite-01",
	[Version.DRAFT_02]: "moq-lite-02",
	[Version.DRAFT_03]: "moq-lite-03",
	[Version.DRAFT_04]: "moq-lite-04",
	[Version.DRAFT_05_WIP]: "moq-lite-05-wip",
};

export function versionName(v: Version): string {
	return VERSION_NAMES[v] ?? `unknown(0x${v.toString(16)})`;
}
