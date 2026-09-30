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
});
