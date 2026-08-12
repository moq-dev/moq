import { expect, test } from "bun:test";
import { SetupOption, SetupOptions } from "./parameters.ts";
import { solicitFromSetup, solicitIntoSetup } from "./solicit.ts";

/**
 * We declare it on every session, so a peer honoring it never sends us an unsolicited
 * PUBLISH_NAMESPACE. The literal 1 is the wire contract, not an implementation detail.
 */
test("we declare that advertisements must be solicited", () => {
	const params = new SetupOptions();
	solicitIntoSetup(params);

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
