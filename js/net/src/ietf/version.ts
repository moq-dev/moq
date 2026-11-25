/**
 * Supported MoQ Transport protocol versions
 */
export const Version = {
	/**
	 * draft-ietf-moq-transport-18
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-18.txt
	 */
	DRAFT_18: 0xff000012,

	/**
	 * draft-ietf-moq-transport-20
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-20.txt
	 */
	DRAFT_20: 0xff000014,

	/**
	 * draft-ietf-moq-transport-22
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.txt
	 */
	DRAFT_22: 0xff000016,

	/**
	 * draft-ietf-moq-transport-23
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-23.txt
	 */
	DRAFT_23: 0xff000017,
} as const;

export type Version = (typeof Version)[keyof typeof Version];

// ALPN / WebTransport subprotocol identifiers for draft versions.
export const ALPN = {
	DRAFT_18: "moqt-18",
	DRAFT_20: "moqt-20",
	DRAFT_22: "moqt-22",
	DRAFT_23: "moqt-23",
} as const;

/**
 * IETF protocol versions used by the ietf/ module.
 * Use this narrower type for version-branched encode/decode to get exhaustive matching.
 */
export type IetfVersion =
	| typeof Version.DRAFT_18
	| typeof Version.DRAFT_20
	| typeof Version.DRAFT_22
	| typeof Version.DRAFT_23;

const VERSION_NAMES: Record<number, string> = {
	[Version.DRAFT_18]: "moq-transport-18",
	[Version.DRAFT_20]: "moq-transport-20",
	[Version.DRAFT_22]: "moq-transport-22",
	[Version.DRAFT_23]: "moq-transport-23",
};

/**
 * Whether `version` is draft-23 or later. The older drafts are listed so a new draft falls
 * forward to draft-23's wire form.
 */
export function isDraft23(version: IetfVersion): boolean {
	switch (version) {
		case Version.DRAFT_18:
		case Version.DRAFT_20:
		case Version.DRAFT_22:
			return false;
		default:
			return true;
	}
}

export function versionName(v: Version): string {
	return VERSION_NAMES[v] ?? `unknown(0x${v.toString(16)})`;
}
