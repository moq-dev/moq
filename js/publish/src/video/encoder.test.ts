import { expect, spyOn, test } from "bun:test";
import type * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Encoder } from "./encoder";
import type { Source } from "./types";

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

test("serve tracks encoder config in its child effect", async () => {
	using _videoEncoder = installFakeVideoEncoder();
	using warn = spyOn(console, "warn").mockImplementation(() => {});

	const frame = new Signal<VideoFrame | undefined>(undefined);
	const source = new Signal<Source | undefined>(undefined);
	const connection = new Signal<Moq.Connection.Established | undefined>(undefined);
	const encoder = new Encoder(frame, source, connection, { enabled: true });
	const effect = new Effect();

	try {
		encoder.serve({ close() {} } as never, effect);
		for (let i = 0; i < 5; i++) await Promise.resolve();

		expect(warn).not.toHaveBeenCalledWith(
			"Effect did not subscribe to any signals; it will never rerun.",
			expect.anything(),
		);
	} finally {
		effect.close();
		encoder.close();
	}
});
