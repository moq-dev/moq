export const Version = {
	DRAFT_01: 0xff0dad01,
	DRAFT_02: 0xff0dad02,
	DRAFT_03: 0xff0dad03,
	DRAFT_04: 0xff0dad04,
	/// Work-in-progress placeholder for lite-05. Not advertised as a
	/// WebTransport subprotocol; callers must opt in explicitly.
	DRAFT_05_WIP: 0xff0dad05,
} as const;

export type Version = (typeof Version)[keyof typeof Version];

/// The WebTransport subprotocol identifier for moq-lite.
/// Version negotiation still happens via SETUP when this is used.
export const ALPN = "moql";

/// The ALPN string for Draft03, which uses ALPN-based version negotiation.
export const ALPN_03 = "moq-lite-03";

/// The ALPN string for Draft04, which uses ALPN-based version negotiation.
export const ALPN_04 = "moq-lite-04";

/// The ALPN string for the work-in-progress Draft05. Intentionally not
/// included in the default WebTransport `protocols` list.
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

/// Whether this version uses a unidirectional Setup stream (moq-lite-05+).
export function hasSetupStream(v: Version): boolean {
	return v === Version.DRAFT_05_WIP;
}

/// Whether this version uses the moq-lite-05 framing bundle: the Track stream
/// (TRACK/TRACK_INFO), a SUBSCRIBE_OK trimmed to the resolved start group plus a
/// distinct SUBSCRIBE_END, and a per-frame timestamp delta.
export function hasTrackStream(v: Version): boolean {
	return v === Version.DRAFT_05_WIP;
}
