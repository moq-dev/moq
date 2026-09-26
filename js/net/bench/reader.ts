/** Sweep frame size and chunk size for one Reader.read of a fragmented frame. */
import { Reader } from "../src/stream.ts";

const frameSizes = [16 * 1024, 256 * 1024, 1024 * 1024];
const chunkSizes = [1200, 16 * 1024, 1024 * 1024];
const bytes = 16 * 1024 * 1024; // Read about this many bytes per case so each row takes similar time.
// Per-byte cost must not grow with frame size, or reassembly has gone quadratic. The margin is
// loose because the smallest frame pays the fixed per-read overhead over the fewest bytes.
const maxSlope = 4;
let checksum = 0;

console.log("frame_bytes,chunk_bytes,chunks,ns_per_byte");
const smallest = new Map<number, number>();
for (const frameSize of frameSizes) {
	for (const chunkSize of chunkSizes) {
		const frame = new Uint8Array(frameSize).fill(1);
		const chunks: Uint8Array[] = [];
		for (let offset = 0; offset < frameSize; offset += chunkSize) {
			chunks.push(frame.subarray(offset, Math.min(offset + chunkSize, frameSize)));
		}

		const iterations = Math.max(1, Math.floor(bytes / frameSize));
		let elapsed = 0;
		for (let index = 0; index < iterations; index++) {
			// Queue the whole frame up front so only the Reader is timed, not the source.
			const stream = new ReadableStream<Uint8Array>({
				start(controller) {
					for (const chunk of chunks) controller.enqueue(chunk);
					controller.close();
				},
			});
			const reader = new Reader(stream);

			const start = performance.now();
			const read = await reader.read(frameSize);
			elapsed += performance.now() - start;

			checksum += read[read.byteLength - 1];
		}

		const ns = (elapsed * 1e6) / (iterations * frameSize);
		console.log(`${frameSize},${chunkSize},${chunks.length},${ns.toFixed(3)}`);

		const baseline = smallest.get(chunkSize);
		if (baseline === undefined) smallest.set(chunkSize, ns);
		else if (ns > baseline * maxSlope) {
			throw new Error(
				`${frameSize} byte frames cost ${ns.toFixed(3)} ns/byte, over ${maxSlope}x ${baseline.toFixed(3)}`,
			);
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
