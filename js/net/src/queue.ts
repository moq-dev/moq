/** An append queue that releases consumed entries without shifting the retained tail. @internal */
export class Queue<T> implements Iterable<T> {
	#items: (T | undefined)[] = [];
	#head = 0;

	constructor(items: Iterable<T> = []) {
		for (const item of items) this.push(item);
	}

	get length(): number {
		return this.#items.length - this.#head;
	}

	get first(): T | undefined {
		return this.#items[this.#head];
	}

	get last(): T | undefined {
		return this.length === 0 ? undefined : this.#items[this.#items.length - 1];
	}

	/** The retained entry `index` places from the front. */
	at(index: number): T | undefined {
		return index < 0 || index >= this.length ? undefined : this.#items[this.#head + index];
	}

	push(item: T): void {
		this.#items.push(item);
	}

	shift(): T | undefined {
		if (this.length === 0) return undefined;
		const item = this.#items[this.#head];
		this.#items[this.#head++] = undefined;
		// Compact geometrically, so removing a frame costs amortized constant time.
		if (this.#head * 2 >= this.#items.length) {
			this.#items = this.#items.slice(this.#head);
			this.#head = 0;
		}
		return item;
	}

	*[Symbol.iterator](): Iterator<T> {
		for (let index = this.#head; index < this.#items.length; index++) yield this.#items[index] as T;
	}
}
