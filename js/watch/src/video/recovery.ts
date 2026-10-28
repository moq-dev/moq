import type { Micro } from "@moq/clock";
import type { Container } from "@moq/hang";

/** Refuse leading pictures whose previous GOP was not delivered to this decoder. @internal */
export class Recovery {
	#needed = true;
	#floor?: Micro;

	/** Arm recovery after media was lost, preserving continuous groups and declared ends. */
	gap(): void {
		this.#needed = true;
	}

	/** Whether this access unit has the references available at the current delivery boundary. */
	accept(frame: Container.ReceivedFrame): boolean {
		if (frame.keyframe) {
			this.#floor = this.#needed ? frame.timestamp : undefined;
			this.#needed = false;
		}
		return this.#floor === undefined || frame.timestamp >= this.#floor;
	}
}
