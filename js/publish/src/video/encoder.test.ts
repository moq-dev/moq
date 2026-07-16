import { expect, spyOn, test } from "bun:test";
import * as Moq from "@moq/net";
import { Signal } from "@moq/signals";
import { Encoder } from "./encoder";

class FakeVideoEncoder {
	state: CodecState = "unconfigured";

	configure(): void {
		this.state = "configured";
	}

	encode(): void {}

	close(): void {
		this.state = "closed";
	}
}

function installFakeVideoEncoder() {
	const original = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: FakeVideoEncoder,
		writable: true,
	});

	return {
		[Symbol.dispose]() {
			if (original) {
				Object.defineProperty(globalThis, "VideoEncoder", original);
			} else {
				Reflect.deleteProperty(globalThis, "VideoEncoder");
			}
		},
	};
}

test("encoding tracks encoder config in its child effect", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	const warn = spyOn(console, "warn").mockImplementation(() => {});

	const track = new Moq.Track.Producer("video/hd").accept();
	const rendition = {
		config: new Signal(undefined),
		track: new Signal<Moq.Track.Producer | undefined>(track),
		close: () => track.close(),
	};
	const broadcast = { video: () => rendition };
	const capture = {
		in: { source: new Signal(undefined) },
		out: { frame: new Signal<VideoFrame | undefined>(undefined) },
	};
	const encoder = new Encoder("video/hd", {
		enabled: true,
		broadcast: broadcast as never,
		capture: capture as never,
	});

	try {
		for (let i = 0; i < 5; i++) await Promise.resolve();

		expect(warn).not.toHaveBeenCalledWith(
			"Effect did not subscribe to any signals; it will never rerun.",
			expect.anything(),
		);
	} finally {
		encoder.close();
		warn.mockRestore();
	}
});
