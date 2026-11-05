import { type Dispose, type Getter, Signal } from "@moq/signals";

type Listener<T> = (value: T) => void;

/** A package-private signal whose listeners hold demand for its producer. */
export class Observed<T> extends Signal<T> {
	readonly wanted = new Signal(false);
	#observers = 0;

	#observe(): Dispose {
		if (++this.#observers === 1) this.wanted.set(true);
		let active = true;
		return () => {
			if (!active) return;
			active = false;
			if (--this.#observers === 0) this.wanted.set(false);
		};
	}

	override subscribe(fn: Listener<T>): Dispose {
		const release = this.#observe();
		const dispose = super.subscribe((value) => fn(value));
		return () => {
			dispose();
			release();
		};
	}

	override changed(): Promise<T>;
	override changed(fn: Listener<T>): Dispose;
	override changed(fn?: Listener<T>): Promise<T> | Dispose {
		if (!fn) return new Promise<T>((resolve) => this.changed(resolve));
		const release = this.#observe();
		const dispose = super.changed((value) => {
			// Schedule a reactive reader's next run before publishing unwanted, so its
			// replacement listener coalesces away the temporary gap in demand.
			try {
				fn(value);
			} finally {
				release();
			}
		});
		return () => {
			dispose();
			release();
		};
	}
}

/** Forward a changing getter without observing its value until a reader asks. */
export class Follow<T> implements Getter<T | undefined> {
	#source: Getter<Getter<T> | undefined>;

	constructor(source: Getter<Getter<T> | undefined>) {
		this.#source = source;
	}

	peek(): T | undefined {
		return this.#source.peek()?.peek();
	}

	subscribe(fn: Listener<T | undefined>): Dispose {
		let inner: Getter<T> | undefined;
		let stopInner: Dispose | undefined;
		const follow = () => {
			const next = this.#source.peek();
			if (next === inner) return;
			const previous = stopInner;
			inner = next;
			stopInner = inner?.subscribe(() => fn(this.peek()));
			previous?.();
		};
		const stopSource = this.#source.subscribe(() => {
			follow();
			fn(this.peek());
		});
		follow();
		return () => {
			stopSource();
			stopInner?.();
		};
	}

	changed(): Promise<T | undefined>;
	changed(fn: Listener<T | undefined>): Dispose;
	changed(fn?: Listener<T | undefined>): Promise<T | undefined> | Dispose {
		if (!fn) return new Promise<T | undefined>((resolve) => this.changed(resolve));
		let active = true;
		let stopInner: Dispose | undefined;
		let stopSource: Dispose | undefined;
		const dispose = () => {
			active = false;
			stopSource?.();
			stopInner?.();
		};
		const changed = () => {
			if (!active) return;
			active = false;
			try {
				fn(this.peek());
			} finally {
				dispose();
			}
		};
		stopSource = this.#source.changed(changed);
		stopInner = this.#source.peek()?.changed(changed);
		return dispose;
	}
}
