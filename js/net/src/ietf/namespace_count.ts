import { MSG_PARAM_NAMESPACE_COUNT, type Parameters, SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The MoQ Namespace Count extension (draft-lcurley-moq-namespace-count-00).
 *
 * moq-transport answers a SUBSCRIBE_NAMESPACE with a REQUEST_OK and then a NAMESPACE per
 * matching namespace, with nothing separating the ones the publisher already had from the
 * ones that showed up a moment later. A subscriber can see that a namespace is present,
 * but never that it is absent.
 *
 * This extension puts the size of that initial set in the response, so the set is complete
 * once that many NAMESPACE messages have arrived. It is moq-lite's `ANNOUNCE_OK.Active
 * Count` on the moq-transport wire.
 *
 * Unknown Message Parameters close the session, so unlike the MoQ Solicit extension this
 * one has to be negotiated: the count only goes to a peer whose SETUP asked for it.
 *
 * @module
 * @internal
 */

/**
 * Whether this version carries the initial set on the SUBSCRIBE_NAMESPACE response stream
 * at all.
 *
 * Draft-14/15 answer with PUBLISH_NAMESPACE requests on streams of their own and have no
 * NAMESPACE message, so there is nothing on the response stream to count.
 *
 * @internal
 */
export function namespaceCountSupported(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15;
}

/**
 * Whether the peer asked us to report the count.
 *
 * Absent and 0 are the same statement: unlike the MoQ Solicit extension, nothing here
 * changes based on whether a peer that wants nothing implements this.
 *
 * @internal
 */
export function namespaceCountFromSetup(params: SetupOptions, version: IetfVersion): boolean {
	if (!namespaceCountSupported(version)) return false;
	const value = params.getVarint(SetupOption.NamespaceCount);
	return value !== undefined && value !== 0n;
}

/**
 * Ask the peer to report the count.
 *
 * Unconditional on every version that can answer, since we open a SUBSCRIBE_NAMESPACE for
 * every prefix we want and each one wants a boundary.
 *
 * @internal
 */
export function namespaceCountIntoSetup(params: SetupOptions, version: IetfVersion) {
	if (!namespaceCountSupported(version)) return;
	params.setVarint(SetupOption.NamespaceCount, 1n);
}

/**
 * The initial set size a SUBSCRIBE_NAMESPACE response reported, or `undefined` on a peer
 * that does not implement the extension. `0n` is a real answer: the prefix is empty.
 *
 * @internal
 */
export function namespaceCountFromResponse(params: Parameters): bigint | undefined {
	return params.vars.get(MSG_PARAM_NAMESPACE_COUNT);
}

/**
 * Report the initial set size on a SUBSCRIBE_NAMESPACE response.
 *
 * @internal
 */
export function namespaceCountIntoResponse(params: Parameters, count: bigint) {
	params.vars.set(MSG_PARAM_NAMESPACE_COUNT, count);
}
