import { expect, test } from "bun:test";
import { Parameters, SetupOption, SetupOptions } from "./parameters.ts";
import {
	namespaceCountFromResponse,
	namespaceCountFromSetup,
	namespaceCountIntoResponse,
	solicitFromSetup,
	solicitIntoSetup,
} from "./solicit.ts";
import { Version } from "./version.ts";

/**
 * We declare it on every session, so a peer honoring it never sends us an unsolicited
 * PUBLISH_NAMESPACE. The literal 1 is the wire contract, not an implementation detail.
 */
test("we declare that advertisements must be solicited", () => {
	const params = new SetupOptions();
	solicitIntoSetup(params, Version.DRAFT_19);

	expect(params.getVarint(SetupOption.Solicit)).toBe(1n);
	expect(solicitFromSetup(params)).toBe(true);
});

/**
 * A peer that has never heard of the extension sends nothing, and must keep getting the
 * unsolicited announcements it expects. Distinct from an explicit 0, which is what lets
 * us tell a peer that ignored our declaration from one that never saw it.
 */
test("an absent option declares nothing", () => {
	expect(solicitFromSetup(new SetupOptions())).toBeUndefined();
});

/**
 * An explicit 0 asks for the same treatment an absent option does, but it is not the same
 * statement: writing the option proves the peer implements this.
 */
test("an explicit zero is a declaration, not an absence", () => {
	const params = new SetupOptions();
	params.setVarint(SetupOption.Solicit, 0n);

	expect(solicitFromSetup(params)).toBe(false);
});

/**
 * A value this draft doesn't define still means "ask me first", so a later revision that
 * says more can't be read as saying nothing.
 */
test("any non-zero value requires solicitation", () => {
	for (const value of [1n, 2n, 0x8000n]) {
		const params = new SetupOptions();
		params.setVarint(SetupOption.Solicit, value);

		expect(solicitFromSetup(params)).toBe(true);
	}
});

/**
 * We ask for the count on every version that has a NAMESPACE to count, and on no other:
 * draft-14/15 answer a SUBSCRIBE_NAMESPACE with PUBLISH_NAMESPACE requests, so the option
 * would promise a count of messages that never arrive on that stream.
 */
test("the count rides only where NAMESPACE does", () => {
	for (const version of [Version.DRAFT_16, Version.DRAFT_17, Version.DRAFT_18, Version.DRAFT_19]) {
		const params = new SetupOptions();
		solicitIntoSetup(params, version);

		expect(params.getVarint(SetupOption.NamespaceCount)).toBe(1n);
		expect(namespaceCountFromSetup(params, version)).toBe(true);
	}

	for (const version of [Version.DRAFT_14, Version.DRAFT_15]) {
		const params = new SetupOptions();
		solicitIntoSetup(params, version);

		expect(params.getVarint(SetupOption.NamespaceCount)).toBeUndefined();
		expect(namespaceCountFromSetup(params, version)).toBe(false);
	}
});

/**
 * A peer that never heard of it asks for nothing, and must keep getting a response without
 * the parameter: an unknown Message Parameter closes its session. An explicit 0 is the same
 * request, since nothing here distinguishes the two.
 */
test("an absent or zero count option asks for nothing", () => {
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
