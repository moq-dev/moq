/**
 * Supported MoQ Transport protocol versions
 */
export const Version = {
	/**
	 * draft-ietf-moq-transport-07
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.txt
	 */
	DRAFT_07: 0xff000007,

	/**
	 * draft-ietf-moq-transport-14
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-14.txt
	 */
	DRAFT_14: 0xff00000e,

	/**
	 * draft-ietf-moq-transport-15
	 * https://www.ietf.org/archive/id/draft-ietf-moq-transport-15.txt
	 */
	DRAFT_15: 0xff00000f,
} as const;

export type Version = (typeof Version)[keyof typeof Version];

// ALPN / WebTransport subprotocol identifiers for draft versions.
export const ALPN = {
	DRAFT_14: "moq-00",
	DRAFT_15: "moqt-15",
} as const;

/**
 * IETF protocol versions used by the ietf/ module.
 * Use this narrower type for version-branched encode/decode to get exhaustive matching.
 */
export type IetfVersion = typeof Version.DRAFT_14 | typeof Version.DRAFT_15;
