import { MSG_PARAM_NAMESPACE_COUNT, type Parameters, SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The MoQ Solicit extension (draft-lcurley-moq-solicit-00).
 *
 * moq-transport says nothing about whether an endpoint expects to be told what we have or
 * to ask for it, so a publisher that waits to be asked is silent against a relay that
 * never asks. This extension lets an endpoint declare, in its SETUP, that advertisements
 * to it must be solicited first.
 *
 * Declaring nothing means "no requirement, tell me unasked", which is what a peer that has
 * never heard of the extension implicitly says.
 *
 * The same draft closes the gap that settling the mechanism exposes: once advertisements
 * arrive as NAMESPACE messages on a stream we opened, nothing separates the ones the
 * publisher already had from the ones that showed up a moment later. The NAMESPACE_COUNT
 * Setup Option asks for a NAMESPACE_COUNT parameter on each SUBSCRIBE_NAMESPACE response
 * reporting the size of that initial set.
 *
 * The two are separate code points because a peer that implements only SOLICIT has never
 * heard of the parameter, and an unknown Message Parameter closes its session.
 *
 * @module
 * @internal
 */

/**
 * What the peer declared, if anything.
 *
 * The three states are distinct, and the difference between the last two is what makes the
 * requirement enforceable:
 *
 * - `undefined`: no option at all. The peer has never heard of the extension, so it cannot
 *   have honored ours and an unsolicited advertisement from it is expected.
 * - `false`: an explicit 0. No requirement of its own, but writing the option at all proves
 *   it implements this, so it is held to ours.
 * - `true`: advertisements to it must be solicited, and likewise held to ours.
 *
 * @internal
 */
export function solicitFromSetup(params: SetupOptions): boolean | undefined {
	const value = params.getVarint(SetupOption.Solicit);
	return value === undefined ? undefined : value !== 0n;
}

/**
 * Declare what we want: solicited advertisements, and a count on each response.
 *
 * Both are unconditional and true of every session: we send SUBSCRIBE_NAMESPACE for each
 * prefix we want, so there is nothing an unsolicited PUBLISH_NAMESPACE could tell us that
 * we will not have asked for, and every one of those asks wants a boundary.
 *
 * @internal
 */
export function solicitIntoSetup(params: SetupOptions, version: IetfVersion) {
	params.setVarint(SetupOption.Solicit, 1n);
	if (namespaceCountSupported(version)) {
		params.setVarint(SetupOption.NamespaceCount, 1n);
	}
}

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
 * Absent and 0 are the same request, unlike the declaration above: nothing here turns on
 * whether a peer that wants no count implements this.
 *
 * @internal
 */
export function namespaceCountFromSetup(params: SetupOptions, version: IetfVersion): boolean {
	if (!namespaceCountSupported(version)) return false;
	const value = params.getVarint(SetupOption.NamespaceCount);
	return value !== undefined && value !== 0n;
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
