import { install } from "./ambient.ts";
import type { Backend } from "./backend.ts";
import { drain } from "./host.ts";
import { Instant, Nano, Wall } from "./types.ts";

type Timer = { at: number; order: number; index: number; ready: () => void };

/** A clock that starts at zero and advances only when a test moves it. */
export class Manual {
	/** The reproducible starting wall time, 2025-01-01T00:00:00Z. */
	static readonly WALL_START = Wall(1_735_689_600_000);
	#now = Instant.zero;
	// Wall time is derived from an anchor so tiny advances don't vanish into an epoch-sized float.
	#wallAnchor = Manual.WALL_START;
	#wallAnchorAt = Instant.zero;
	#timers: Timer[] = [];
	#order = 0;
	#revision = 0;
	#fired = 0;
	#firing = false;
	#armed?: () => void;
	#backend: Backend = {
		now: () => this.#now,
		wall: () => this.wall(),
		origin: () => this.#wallAnchor - Nano.toMilli(Nano(this.#wallAnchorAt)),
		arm: (at, ready) => {
			const timer = { at, order: this.#order++, index: this.#timers.length, ready };
			this.#timers.push(timer);
			this.#up(timer.index);
			this.#revision++;
			this.#fire();
			this.#armed?.();
			return () => {
				this.#remove(timer);
			};
		},
	};

	/** Install this clock in the current realm and return an idempotent restore function. */
	install(): () => void {
		return install(this.#backend);
	}

	/** Read this clock's instant without installing it. */
	now(): Instant {
		return this.#now;
	}

	/** Read this clock's wall time without installing it. */
	wall(): Wall {
		return Wall.add(this.#wallAnchor, Instant.durationSince(this.#now, this.#wallAnchorAt));
	}

	/** Move the instant and wall time forward by a duration. */
	advance(duration: Nano): void {
		this.#now = Instant.add(this.#now, duration);
		this.#fire();
	}

	/** Advance, then drain due timers and the microtasks they schedule. */
	async advanceAsync(duration: Nano): Promise<void> {
		this.advance(duration);
		for (;;) {
			const revision = this.#revision;
			await drain();
			this.#fire();
			if (revision === this.#revision) return;
		}
	}

	/**
	 * Wait until a timer is armed, advance exactly to the earliest deadline, then drain like
	 * `advanceAsync` and return the instant reached. Awaits forever if no timer is ever armed.
	 */
	async fire(): Promise<Instant> {
		if (this.#firing) throw new Error("fire already pending");
		this.#firing = true;
		const fired = this.#fired;
		try {
			for (;;) {
				// Lets a pending microtask chain arm an earlier timer before choosing the deadline.
				await drain();
				// arm() runs an already-due timer at once, so it never waits in the heap.
				const at = this.#fired !== fired ? this.#now : this.#timers[0]?.at;
				if (at !== undefined) {
					await this.advanceAsync(Instant.durationSince(Instant(at), this.#now));
					return this.#now;
				}
				// A host task, such as I/O, may arm the timer later than any drain can wait for.
				await new Promise<void>((resolve) => {
					this.#armed = resolve;
				});
				this.#armed = undefined;
			}
		} finally {
			this.#firing = false;
		}
	}

	/** Set wall time forward or backward without passing monotonic time. */
	stepWall(wall: Wall): void {
		this.#wallAnchor = Wall(wall);
		this.#wallAnchorAt = this.#now;
	}

	#before(a: Timer, b: Timer): boolean {
		return a.at < b.at || (a.at === b.at && a.order < b.order);
	}

	#swap(a: number, b: number): void {
		const left = this.#timers[a];
		const right = this.#timers[b];
		this.#timers[a] = right;
		this.#timers[b] = left;
		left.index = b;
		right.index = a;
	}

	#up(index: number): void {
		while (index > 0) {
			const parent = (index - 1) >> 1;
			if (!this.#before(this.#timers[index], this.#timers[parent])) return;
			this.#swap(index, parent);
			index = parent;
		}
	}

	#down(index: number): void {
		for (;;) {
			let child = index * 2 + 1;
			if (child >= this.#timers.length) return;
			if (child + 1 < this.#timers.length && this.#before(this.#timers[child + 1], this.#timers[child])) child++;
			if (!this.#before(this.#timers[child], this.#timers[index])) return;
			this.#swap(index, child);
			index = child;
		}
	}

	#remove(timer: Timer): void {
		if (timer.index < 0) return;
		const index = timer.index;
		const last = this.#timers.pop();
		timer.index = -1;
		if (!last || last === timer) return;
		this.#timers[index] = last;
		last.index = index;
		this.#up(index);
		this.#down(last.index);
	}

	#fire(): void {
		for (;;) {
			const first = this.#timers[0];
			if (!first || first.at > this.#now) return;
			this.#remove(first);
			this.#revision++;
			this.#fired++;
			first.ready();
		}
	}
}
