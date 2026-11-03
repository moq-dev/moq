import * as Catalog from "@moq/hang/catalog";

// The window `delay` is measured over before it is reported, in microseconds of the rendition's own
// media. Mirrors `RATE_WINDOW` in `moq_mux::catalog::Estimator`.
const WINDOW = 1_000_000;

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

	// Forget `estimator`'s frames, so a rendition that stopped no longer holds the others back. It may
	// resume at another offset, so its next window starts afresh.
	leave(estimator: Estimator): void {
		this.#highs.delete(estimator);
		estimator.restart();
	}
}

// One rendition's measured catalog `delay`, in whole milliseconds. Mirrors
// `moq_mux::catalog::Estimator`, which measures it only for a rendition whose config supplies no
// `delay` hint; the js/publish encoders supply none, so theirs is always measured.
//
// Each window of a second of the rendition's media measures the least its frames trailed the other
// renditions, and the delay is the largest of those. A constant offset is in every window, so it
// shows. Lateness only some frames have, such as B-frame reorder or an interleave, has a frame that
// trails nothing in every window, so it never does: the receiver measures it, and counting it here
// too would buffer it twice. Since a window has to close first, a rendition that starts on a frame
// trailing by the reorder never reports it, and the delay only ever rises, as the catalog requires.
export class Estimator {
	#delay = 0;
	// The rendition's highest timestamp, and what it was when the open window started.
	#high?: number;
	#open?: number;
	// The least trail in the open window, in microseconds.
	#trail?: number;

	// The catalog field measured so far, absent until nonzero.
	get estimate(): { delay?: Catalog.U53 } {
		return { delay: this.#delay ? Catalog.u53(this.#delay) : undefined };
	}

	// Place a frame written at `timestamp` in the broadcast's `order`. Returns whether the delay rose.
	write(timestamp: number, order: Order): boolean {
		// A frame with no other rendition to trail trails nothing, so a window holding frames from
		// before another rendition joined never reports the trail of the few after it.
		const behind = order.write(this, timestamp) ?? 0;
		const trail = Math.min(this.#trail ?? behind, behind);
		this.#trail = trail;

		this.#high = Math.max(this.#high ?? timestamp, timestamp);
		this.#open ??= this.#high;
		if (this.#high - this.#open < WINDOW) return false;

		this.#open = this.#high;
		this.#trail = undefined;

		const delay = Math.ceil(trail / 1000);
		if (delay <= this.#delay) return false;

		this.#delay = delay;
		return true;
	}

	// Measure the next window afresh, keeping the delay already measured.
	restart(): void {
		this.#high = undefined;
		this.#open = undefined;
		this.#trail = undefined;
	}
}
