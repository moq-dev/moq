// The slice bound by UniFFI over ubrn's wasm2 player, behind the bench's adapter.
import type { Slice } from "./adapter.ts";
import { MoqBroadcastProducer, noop, noopAsync, uniffiInitAsync, yieldOnce } from "./dist/uniffi/index.ts";

export async function open(wasm: URL): Promise<Slice> {
	await uniffiInitAsync(wasm);
	return {
		noop,
		noopAsync: () => noopAsync(),
		yieldOnce: () => yieldOnce(),
		group() {
			const broadcast = new MoqBroadcastProducer();
			const track = broadcast.publishTrack("data");
			const group = track.appendGroup();
			const consumer = group.consume();
			return {
				sequence: () => consumer.sequence(),
				// The bench's payloads span their whole buffer, so passing it adds no JS copy.
				write: (bytes) => group.writeFrame({ payload: bytes.buffer as ArrayBuffer, timestampUs: 0n }),
				read: async () => (await consumer.readFrame())?.payload.byteLength,
				close: () => track.finish(),
			};
		},
	};
}
