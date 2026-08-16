import { SetupOption, type SetupOptions } from "./parameters.ts";

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
 * Declare that advertisements to us must be solicited.
 *
 * Unconditional, and true of every session: we send SUBSCRIBE_NAMESPACE for each prefix
 * we want, so there is nothing an unsolicited PUBLISH_NAMESPACE could tell us that we
 * will not have asked for.
 *
 * @internal
 */
export function solicitIntoSetup(params: SetupOptions) {
	params.setVarint(SetupOption.Solicit, 1n);
}
