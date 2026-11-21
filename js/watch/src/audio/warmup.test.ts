import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { createAudioBuffer } from "./buffer";
import type { InitShared } from "./render";
import { SharedRingBuffer } from "./shared-ring-buffer";
import { Warmup } from "./warmup";

test("decoder warm-up drops only the configured initial callbacks", () => {
	const warmup = new Warmup(3);

	expect([warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop(), warmup.drop()]).toEqual([
		true,
		true,
		true,
		false,
		false,
	]);
});

// The legacy decode loop, as `#play` and the decoder's output callback run it: a 40 ms floor of 20 ms
// frames. Gated, the warm-up inputs would spend the ring's whole refill on audio that is discarded,
// leaving the ring empty and the gate waiting on it forever.
test("warm-up inputs skip the ring's backpressure, so the ring still fills to its floor", () => {
	const sent: InitShared[] = [];
	const worklet = { port: { postMessage: (msg: InitShared) => sent.push(msg) } } as unknown as AudioWorkletNode;
	const buffer = createAudioBuffer(worklet, 1, 1000, 40, true);
	const signal = new AbortController().signal;
	const warmup = new Warmup(3);
	try {
		const decoded: Time.Micro[] = [];
		for (let timestamp = 0; ; timestamp += 20_000) {
			if (!warmup.ungated() && Bun.peek.status(buffer.wait(timestamp as Time.Micro, signal)) === "pending") break;
			decoded.push(timestamp as Time.Micro);
		}
		for (const timestamp of decoded) {
			if (!warmup.drop()) buffer.insert(timestamp, [new Float32Array(20)]);
		}
		expect(new SharedRingBuffer(sent[0]).stalled).toBe(false);
	} finally {
		buffer.close();
	}
});
