import { expect, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import { intoSetup, offered } from "./extensions.ts";

test("every extension is offered by default", () => {
	expect(offered()).toEqual({ auth: true, solicit: true });
	expect(offered({ solicit: false })).toEqual({ auth: true, solicit: false });
});

test("only the offered extensions reach the SETUP", () => {
	const all = new Ietf.SetupOptions();
	intoSetup(all, offered(), Ietf.Version.DRAFT_18);
	expect(Ietf.solicitFromSetup(all)).toBe(true);
	expect(Ietf.Auth.fromSetup(all, Ietf.Version.DRAFT_18)).toBe(true);

	const none = new Ietf.SetupOptions();
	intoSetup(none, offered({ auth: false, solicit: false }), Ietf.Version.DRAFT_18);
	expect(Ietf.solicitFromSetup(none)).toBeUndefined();
	expect(Ietf.Auth.fromSetup(none, Ietf.Version.DRAFT_18)).not.toBe(true);
});
