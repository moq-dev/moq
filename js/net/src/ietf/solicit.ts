import { SetupOption, type SetupOptions } from "./parameters.ts";

/**
 * The MoQ Solicit extension (draft-lcurley-moq-solicit-00).
 *
 * moq-transport says nothing about whether an endpoint expects to be told what we have or
 * to ask for it, so a publisher that waits to be asked is silent against a relay that
 * never asks. This extension lets each side declare, in its SETUP, that advertisements to
 * it must be solicited first.
 *
 * Declaring nothing means "no requirements, tell me unasked", which is what a peer that
 * has never heard of the extension implicitly says. We declare nothing ourselves: a
 * browser connection can subscribe at any time, so there is nothing we could rule out
 * honestly.
 *
 * @internal
 */
export type Solicit = {
	/** Advertisements must be solicited: the peer asks with SUBSCRIBE_NAMESPACE, so don't send it PUBLISH_NAMESPACE. */
	announce: boolean;
};

const ANNOUNCE = 0x1n;

/**
 * Read the SOLICIT Setup Option out of a peer's SETUP. Absent means no requirements,
 * and unknown bits are ignored so later flags stay additive.
 *
 * @internal
 */
export function solicitFromSetup(params: SetupOptions): Solicit {
	const bits = params.getVarint(SetupOption.Solicit) ?? 0n;

	return {
		announce: (bits & ANNOUNCE) !== 0n,
	};
}
