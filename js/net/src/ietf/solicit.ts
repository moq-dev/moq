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
 * Whether the peer requires advertisements to be solicited, from its SETUP.
 *
 * Absent is the same as 0, which is what a peer unaware of the extension declares: no
 * requirement, so we advertise unasked. Any non-zero value means it will ask.
 *
 * @internal
 */
export function solicitFromSetup(params: SetupOptions): boolean {
	return (params.getVarint(SetupOption.Solicit) ?? 0n) !== 0n;
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
