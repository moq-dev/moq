import { Container } from "@moq/hang";
import type * as Moq from "@moq/wasm";
import type { Time } from "@moq/wasm";

type Source = Container.Legacy.Source;

// Helper to encode frames into a track, one group per keyframe. Moved out of
// @moq/hang so that hang stays serialization-only (no networking model); the
// frame encoding itself still lives in hang as `Container.Legacy.encodeFrame`.
export class Producer {
	#track: Moq.TrackProducer;
	#group?: Moq.Group;

	constructor(track: Moq.TrackProducer) {
		this.#track = track;
	}

	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame(Container.Legacy.encodeFrame(data, timestamp));
	}

	close(err?: Error) {
		this.#track.close(err);
		this.#group?.close();
	}
}
