import { describe, expect, it } from "bun:test";
import { Handover } from "./handover";

describe("handover", () => {
	it("does not take over for the first subscription", () => {
		const handover = new Handover();
		handover.opened();
		expect(handover.takeover()).toBeUndefined();
	});

	it("takes over on the first frame of a replacement, and only that frame", () => {
		const handover = new Handover();
		handover.opened();
		handover.takeover();

		handover.opened();
		expect(handover.takeover()).toEqual({ end: undefined });
		// The rest of the replacement's audio is its own; only its first frame drops the tail.
		expect(handover.takeover()).toBeUndefined();
	});

	it("takes over once no matter how many subscriptions came and went", () => {
		// The decoder effect can rerun several times before a frame decodes (a rendition swap
		// landing mid-reconnect, say). The ring only needs dropping once, at the survivor's frame.
		const handover = new Handover();
		handover.opened();
		handover.opened();
		handover.opened();

		expect(handover.takeover()).toEqual({ end: undefined });
		expect(handover.takeover()).toBeUndefined();
	});

	// The ring retires a declared end at the takeover, which would drop one the replacement read
	// before its first output, so only the predecessor's end is lost.
	it("carries the end the replacement declared before its first frame", () => {
		const handover = new Handover();
		handover.opened();
		handover.declared(100);
		expect(handover.takeover()).toBeUndefined();

		handover.opened();
		handover.declared(200);
		expect(handover.takeover()).toEqual({ end: 200 });
		handover.declared(300);
		expect(handover.takeover()).toBeUndefined();
	});

	// The next utterance retires the end before the replacement's first output, so it stays retired.
	it("drops a replacement's end once its next utterance begins", () => {
		const handover = new Handover();
		handover.opened();
		handover.opened();
		handover.declared(200);
		handover.declared(undefined);
		expect(handover.takeover()).toEqual({ end: undefined });
	});
});
