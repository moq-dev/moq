import { expect, test } from "bun:test";
import {
	namespaceCountFromResponse,
	namespaceCountFromSetup,
	namespaceCountIntoResponse,
	namespaceCountIntoSetup,
} from "./namespace_count.ts";
import { Parameters, SetupOption, SetupOptions } from "./parameters.ts";
import { Version } from "./version.ts";

/**
 * We ask on every version that can answer, so each SUBSCRIBE_NAMESPACE we open comes back
 * with a boundary. The literal 1 is the wire contract, not an implementation detail.
 */
test("we ask for the initial set size", () => {
	for (const version of [Version.DRAFT_16, Version.DRAFT_17, Version.DRAFT_18, Version.DRAFT_19]) {
		const params = new SetupOptions();
		namespaceCountIntoSetup(params, version);

		expect(params.getVarint(SetupOption.NamespaceCount)).toBe(1n);
		expect(namespaceCountFromSetup(params, version)).toBe(true);
	}
});

/**
 * Draft-14/15 have no NAMESPACE message, so we neither ask nor answer: the option would
 * promise a count of messages that never arrive on that stream.
 */
test("versions without NAMESPACE ask for nothing", () => {
	for (const version of [Version.DRAFT_14, Version.DRAFT_15]) {
		const params = new SetupOptions();
		namespaceCountIntoSetup(params, version);

		expect(params.getVarint(SetupOption.NamespaceCount)).toBeUndefined();
		expect(namespaceCountFromSetup(params, version)).toBe(false);
	}
});

/**
 * A peer that never heard of the extension asks for nothing, and must keep getting a
 * response without the parameter: an unknown Message Parameter closes its session.
 */
test("an absent option asks for nothing", () => {
	expect(namespaceCountFromSetup(new SetupOptions(), Version.DRAFT_19)).toBe(false);

	const zero = new SetupOptions();
	zero.setVarint(SetupOption.NamespaceCount, 0n);
	expect(namespaceCountFromSetup(zero, Version.DRAFT_19)).toBe(false);
});

/**
 * An empty initial set is a real answer, so 0 must survive the round trip as `0n` rather
 * than collapsing into "the peer said nothing".
 */
test("an empty initial set stays distinct from an absent one", () => {
	const params = new Parameters();
	expect(namespaceCountFromResponse(params)).toBeUndefined();

	namespaceCountIntoResponse(params, 0n);
	expect(namespaceCountFromResponse(params)).toBe(0n);

	namespaceCountIntoResponse(params, 7n);
	expect(namespaceCountFromResponse(params)).toBe(7n);
});
