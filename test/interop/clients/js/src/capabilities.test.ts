import { expect, test } from "bun:test";
import { type Capabilities, missing } from "./capabilities";

const player: Capabilities = {
	WebSocket: true,
	VideoDecoder: true,
	AudioDecoder: true,
	AudioWorkletNode: true,
	"decode avc1.42001f": true,
	"decode vp8": true,
	"decode opus": true,
};

test("a fallback player does not require the publisher's capture or encoder APIs", () => {
	expect(missing(player, "player")).toEqual([]);
	expect(missing(player, "publisher")).toContain("MediaStreamTrackProcessor");
	expect(missing(player, "publisher")).toContain("WebTransport");
});

test("unsupported native decode refuses the player with the missing API named", () => {
	expect(missing({ ...player, "decode opus": false }, "player")).toEqual(["decode opus"]);
	expect(missing({ ...player, VideoDecoder: false }, "player")).toEqual(["VideoDecoder"]);
});
