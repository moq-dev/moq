export const Version = {
	DRAFT_01: 0xff0dad01,
	DRAFT_02: 0xff0dad02,
	DRAFT_03: 0xff0dad03,
} as const;

export type Version = (typeof Version)[keyof typeof Version];

/// The WebTransport subprotocol identifier for moq-lite.
/// Version negotiation still happens via SETUP when this is used.
export const ALPN = "moql";

/// The ALPN string for Draft03, which uses ALPN-based version negotiation.
export const ALPN_03 = "moq-lite-03";

const VERSION_NAMES: Record<number, string> = {
	[Version.DRAFT_01]: "moq-lite-01",
	[Version.DRAFT_02]: "moq-lite-02",
	[Version.DRAFT_03]: "moq-lite-03",
};

export function versionName(v: Version): string {
	return VERSION_NAMES[v] ?? `unknown(0x${v.toString(16)})`;
}
