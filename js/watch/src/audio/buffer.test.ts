import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { createAudioBuffer } from "./buffer";
import type { InitShared } from "./render";
import { SharedRingBuffer } from "./shared-ring-buffer";

describe("SharedAudioBuffer", () => {
	// The worklet keeps reading the old ring until `init-shared` lands, so a rise that grows the ring
	// has to park that one too, or audio drains ahead of video in between.
	it("parks the ring the worklet still reads when a rise grows it", () => {
		const sent: InitShared[] = [];
		const worklet = { port: { postMessage: (msg: InitShared) => sent.push(msg) } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100);
		try {
			const old = new SharedRingBuffer(sent[0]);
			buffer.insert(0 as Time.Micro, [new Float32Array(100).fill(1.0)]);
			expect(old.read([new Float32Array(50)])).toBe(50);

			buffer.setLatency(1000);
			expect(sent.length).toBe(2);
			expect(old.stalled).toBe(true);
			expect(old.read([new Float32Array(50)])).toBe(0);
			expect(new SharedRingBuffer(sent[1]).stalled).toBe(true);
		} finally {
			buffer.close();
		}
	});

	// A decode loop can reach `wait` on a buffer a shape change already closed. Nothing advances a
	// closed buffer, so a gated wait would never settle and would hold the decoder effect's rerun.
	it("does not gate a wait on a closed buffer", async () => {
		const worklet = { port: { postMessage: () => {} } } as unknown as AudioWorkletNode;
		const buffer = createAudioBuffer(worklet, 1, 1000, 100, true);
		buffer.insert(0 as Time.Micro, [new Float32Array(200)]);
		const settles = (wait: Promise<void>) =>
			Promise.race([
				wait.then(() => true),
				new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 0)),
			]);

		// Open, a frame this far ahead of the playhead is held.
		expect(await settles(buffer.wait(10_000_000 as Time.Micro))).toBe(false);

		buffer.close();
		expect(await settles(buffer.wait(10_000_000 as Time.Micro))).toBe(true);
	});
});
