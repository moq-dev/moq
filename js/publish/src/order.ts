import * as Catalog from "@moq/hang/catalog";

// The highest timestamp each rendition of one broadcast has written. A frame written behind media
// another rendition already wrote has to be waited for by a consumer playing both, by however far it
// trails. Measured from timestamps alone, so neither a capture clock nor a stalled encoder reads as
// delay unless the frames really reach the transport out of order.
//
// Every js/publish timestamp is the installed clock in microseconds, so they compare across
// renditions and one order can be shared by a whole broadcast.
export class Order {
	#highs = new Map<Estimator, number>();

	// Record `estimator`'s frame at `timestamp` and return how far it trails another rendition's
	// highest, or undefined when it trails none.
	write(estimator: Estimator, timestamp: number): number | undefined {
		let others: number | undefined;
		for (const [seat, high] of this.#highs) {
			if (seat !== estimator && (others === undefined || high > others)) others = high;
		}
		const high = this.#highs.get(estimator);
		if (high === undefined || timestamp > high) this.#highs.set(estimator, timestamp);
		return others !== undefined && others > timestamp ? others - timestamp : undefined;
	}

	// Forget `estimator`'s frames, so a rendition that stopped no longer holds the others back.
	leave(estimator: Estimator): void {
		this.#highs.delete(estimator);
	}
}

// One rendition's catalog `delay`, a lifetime maximum in whole milliseconds.
// Mirrors `moq_mux::catalog::Estimator`.
export class Estimator {
	#delay = 0;

	// The catalog field measured so far, absent until nonzero.
	get estimate(): { delay?: Catalog.U53 } {
		return { delay: this.#delay ? Catalog.u53(this.#delay) : undefined };
	}

	// Place a frame written at `timestamp` in the broadcast's `order`. Returns whether the delay rose.
	write(timestamp: number, order: Order): boolean {
		const behind = order.write(this, timestamp);
		if (behind === undefined) return false;

		const delay = Math.ceil(behind / 1000);
		if (delay <= this.#delay) return false;

		this.#delay = delay;
		return true;
	}
}
