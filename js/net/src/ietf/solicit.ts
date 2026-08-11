import { SetupOption, type SetupOptions } from "./parameters.ts";

/**
 * The MoQ Solicit extension (draft-lcurley-moq-solicit-00).
 *
 * moq-transport says nothing about which of the two namespace discovery messages an
 * endpoint expects, so a publisher that waits to be asked is silent against a relay that
 * never asks, and one that announces unprompted floods a peer that only publishes. This
 * extension lets each side declare, in its SETUP, what it requires to be solicited.
 *
 * Declaring nothing means "no requirements, send me both", which is what a peer that has
 * never heard of the extension implicitly says. We declare nothing ourselves: a browser
 * connection can publish and subscribe at any time, so there is nothing we could rule
 * out honestly.
 *
 * @internal
 */
export type Solicit = {
	/** Advertisements must be solicited: the peer asks with SUBSCRIBE_NAMESPACE, so don't send it PUBLISH_NAMESPACE. */
	announce: boolean;

	/** Soliciting is pointless: the peer advertises no namespaces, so don't send it SUBSCRIBE_NAMESPACE. */
	interest: boolean;
};

const ANNOUNCE = 0x1n;
const INTEREST = 0x2n;

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
		interest: (bits & INTEREST) !== 0n,
	};
}
