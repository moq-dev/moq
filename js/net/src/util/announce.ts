import { Signal } from "@moq/signals";
import type { Producer } from "../announced.ts";

/** Close an announcement stream once no consumer wants it: how a reader cancels the request. */
export function closeUnwanted(producer: Producer): void {
	void (async () => {
		while (producer.wanted.peek() && producer.closed.peek() === undefined) {
			await Signal.race(producer.wanted, producer.closed);
		}
		producer.close();
	})();
}
