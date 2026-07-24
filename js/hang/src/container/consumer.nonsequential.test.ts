import { expect, test } from "bun:test";
import { Group, Time, Track, Varint } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Format as LegacyFormat } from "./legacy.ts";
import type { Frame } from "./types.ts";

// Kept separate from consumer.test.ts so this regression needs only the Legacy container
// format, with no CMAF init/setup -- the non-sequential-group-id coverage stays self-contained.

function encodeLegacyFrame(timestamp: Time.Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

test("Consumer delivers a non-sequential-gap group's frames incrementally, not batched at group completion", async () => {
	// Some encoders number groups non-sequentially (large, non-+1 jumps). A prior bug gated per-frame
	// delivery on `sequence === #active` and fell back to `#active = prevSequence + 1` when the
	// next group wasn't buffered yet. With non-sequential ids that `+1` is a phantom the real next group
	// never matches, so its frames were held until the group *closed*, then flushed in a burst
	// (a ~1s stall-then-dump in the player). Frames must instead surface as they arrive.
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format: new LegacyFormat(), latency: 500 as Time.Milli });

	// Group A at a large sequence, completed so the cursor advances (arming the old
	// `+1` phantom on #active).
	const a = new Group.Producer(1_000_000);
	a.writeFrame({ payload: encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0x01])), timestamp: Time.Timestamp.now() });
	a.close();
	track.writeGroup(a);

	// Drain A: its frame, then its group-done marker.
	const firstFrame = await consumer.next();
	expect(firstFrame?.frame?.payload).toEqual(new Uint8Array([0x01]));
	await consumer.next();

	// Park next() BEFORE the next group's frames arrive. The live streaming case.
	const pending = consumer.next();

	// Open group B at a large jump (+90_000, NOT +1) and write ONE frame WITHOUT closing it.
	const b = new Group.Producer(1_090_000);
	track.writeGroup(b);
	b.writeFrame({ payload: encodeLegacyFrame(1_000_000 as Time.Micro, new Uint8Array([0x02])), timestamp: Time.Timestamp.now() });

	// The frame must surface while B is still open. Before the fix, next() stayed parked
	// (B.sequence !== the phantom #active, and B was never closed, so no notify fired) and this
	// would time out.
	const result = await Promise.race([
		pending,
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 500)),
	]);

	expect(result).not.toBe("timeout");
	expect((result as { frame?: Frame } | undefined)?.frame?.payload).toEqual(new Uint8Array([0x02]));

	consumer.close();
});
