import { expect, test } from "bun:test";
import type * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { Encoder } from "./encoder";
import type { Source } from "./types";

test("serve tracks encoder config in its child effect", async () => {
	const OriginalVideoEncoder = globalThis.VideoEncoder;
	const originalWarn = console.warn;
	const warnings: unknown[][] = [];

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

	globalThis.VideoEncoder = FakeVideoEncoder as unknown as typeof VideoEncoder;
	console.warn = (...args: unknown[]) => warnings.push(args);

	const frame = new Signal<VideoFrame | undefined>(undefined);
	const source = new Signal<Source | undefined>(undefined);
	const connection = new Signal<Moq.Connection.Established | undefined>(undefined);
	const encoder = new Encoder(frame, source, connection, { enabled: true });
	const effect = new Effect();

	try {
		encoder.serve({ close() {} } as never, effect);
		for (let i = 0; i < 5; i++) await Promise.resolve();

		expect(warnings.map(([message]) => message)).not.toContain(
			"Effect did not subscribe to any signals; it will never rerun.",
		);
	} finally {
		effect.close();
		encoder.close();
		console.warn = originalWarn;
		globalThis.VideoEncoder = OriginalVideoEncoder;
	}
});
