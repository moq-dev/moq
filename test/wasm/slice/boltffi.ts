// The slice bound by BoltFFI, behind the bench's adapter.
import type { Slice } from "./adapter.ts";
import init, { MoqBroadcastProducer, noop, noopAsync, yieldOnce } from "./dist/boltffi/moq_ffi_gen.js";

export async function open(wasm: URL): Promise<Slice> {
	await init(await fetch(wasm));
	return {
		noop,
		noopAsync: () => noopAsync(),
		yieldOnce: () => yieldOnce(),
		group() {
			const broadcast = MoqBroadcastProducer.new();
			const track = broadcast.publishTrack("data");
			const group = track.appendGroup();
			const consumer = group.consume();
			return {
				sequence: () => consumer.sequence(),
				write: (bytes) => group.writeFrame({ payload: bytes, timestampUs: 0n }),
				read: async () => (await consumer.readFrame())?.payload.byteLength,
				close: () => track.finish(),
			};
		},
	};
}
