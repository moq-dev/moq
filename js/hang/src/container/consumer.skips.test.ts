import { expect, spyOn, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Format } from "./legacy.ts";

for (const count of [1, 16, 256]) {
	test(`Consumer summarizes ${count} skipped groups once per catch-up`, async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const track = new Track.Producer("tone");
		const maxDelay = new Signal(Milli(30_000));
		const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
			format: new Format("data"),
			maxDelay,
		});
		const write = (sequence: number, timestamp: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: Varint.encode(timestamp), timestamp: Time.Timestamp.now() });
			return group;
		};
		const buffered = async (timestamp: number) => {
			while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
		};
		try {
			// Sparse sequence numbers make the count differ from the sequence range.
			for (let i = 0; i < count; i++) write(i * 2, i * 1000);
			await buffered(count - 1);
			expect(warn).not.toHaveBeenCalled();
			maxDelay.set(Milli.zero);
			const newest = write(count * 2, count * 1000);
			await buffered(count);
			expect(warn).toHaveBeenCalledTimes(1);
			expect(warn).toHaveBeenLastCalledWith(`skipping slow groups: track=tone 0 -> ${count * 2} count=${count}`);
			expect(consumer.buffered.peek()[0]?.start).toBe(Milli(count));
			// The retained group is still delivered, and another catch-up reports separately.
			expect(await consumer.next()).toMatchObject({ kind: "frame", group: count * 2 });
			newest.writeFrame({ payload: Varint.encode(count * 1000), timestamp: Time.Timestamp.now() });
			write(count * 2 + 2, (count + 1) * 1000);
			await buffered(count + 1);
			expect(warn).toHaveBeenCalledTimes(2);
			expect(warn).toHaveBeenLastCalledWith(
				`skipping slow groups: track=tone ${count * 2} -> ${count * 2 + 2} count=1`,
			);
		} finally {
			consumer.close();
			track.finish();
			track.close();
			warn.mockRestore();
		}
	});
}
