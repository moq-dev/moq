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

/// Whether Hop IDs (ANNOUNCE / ANNOUNCE_OK) and `Exclude Hop` (ANNOUNCE_INTEREST) are
/// carried as fixed-width 64-bit integers rather than varints. Added in lite-05: Hop IDs
/// are randomly assigned, so a varint would almost never be shorter, and the fixed width
/// buys the full 64-bit space (a varint caps at 62 bits).
export function hopsFixedWidth(version: Version): boolean {
	// Explicitly list older versions so future versions default to fixed-width.
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

/// Whether ANNOUNCE_BROADCAST carries a per-broadcast Epoch varint (after the suffix,
/// before the hop chain). Added in lite-05 so a consumer can tell a newer instance of a
/// broadcast from an older one. Older versions omit the field.
export function hasBroadcastEpoch(version: Version): boolean {
	// Explicitly list older versions so future versions default to carrying the epoch.
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
