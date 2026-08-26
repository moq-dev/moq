import { describe, expect, it } from "bun:test";
import { Handover } from "./handover";

describe("handover", () => {
	it("does not take over for the first subscription", () => {
		const handover = new Handover();
		handover.opened();
		expect(handover.takeover()).toBe(false);
	});

	it("takes over on the first frame of a replacement, and only that frame", () => {
		const handover = new Handover();
		handover.opened();
		handover.takeover();

		handover.opened();
		expect(handover.takeover()).toBe(true);
		// The rest of the replacement's audio is its own; only its first frame drops the tail.
		expect(handover.takeover()).toBe(false);
	});

	it("takes over once no matter how many subscriptions came and went", () => {
		// The decoder effect can rerun several times before a frame decodes (a rendition swap
		// landing mid-reconnect, say). The ring only needs dropping once, at the survivor's frame.
		const handover = new Handover();
		handover.opened();
		handover.opened();
		handover.opened();

		expect(handover.takeover()).toBe(true);
		expect(handover.takeover()).toBe(false);
	});
});
