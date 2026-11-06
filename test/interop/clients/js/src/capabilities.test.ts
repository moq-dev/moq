import { expect, test } from "bun:test";
import { type Capabilities, capturePath, missing, NO_CAPTURE } from "./capabilities";

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
	expect(missing(player, "publisher")).toContain(NO_CAPTURE);
	expect(missing(player, "publisher")).toContain("VideoEncoder");
});

test("a publisher needs the capture path production takes, not the window global", () => {
	const firefox = { requestVideoFrameCallback: true };
	expect(capturePath(firefox)).toBe("element");
	expect(missing(firefox, "publisher")).not.toContain(NO_CAPTURE);

	// A worker API is only usable when the track can be handed to it.
	expect(capturePath({ ...firefox, "worker MediaStreamTrackProcessor": true })).toBe("element");
	expect(
		capturePath({ ...firefox, "worker MediaStreamTrackProcessor": true, "MediaStreamTrack transfer": true }),
	).toBe("worker");
	expect(capturePath({ ...firefox, MediaStreamTrackProcessor: true })).toBe("window");
	expect(capturePath({})).toBeUndefined();
});

test("unsupported native decode refuses the player with the missing API named", () => {
	expect(missing({ ...player, "decode opus": false }, "player")).toEqual(["decode opus"]);
	expect(missing({ ...player, VideoDecoder: false }, "player")).toEqual(["VideoDecoder"]);
});
