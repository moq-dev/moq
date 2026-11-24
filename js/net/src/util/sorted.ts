// Chunks stay between LOAD / 2 and 2 * LOAD entries, so an insert or delete shifts one chunk
// and the chunk index rather than the whole collection.
const LOAD = 256;

/**
 * Distinct values ordered by a key, with neighbor lookups by key.
 *
 * Kept in sorted chunks, the layout of Python's `sortedcontainers`: a lookup is two binary
 * searches, and an insert or delete costs O(log n + LOAD + n / LOAD) moves. Callers must not
 * change a stored value's key, and must not mutate the list while iterating {@link from}.
 *
 * @internal
 */
export class Sorted<K, V> {
	#chunks: V[][] = [];
	#size = 0;
	// The position the last #seek found.
	#c = 0;
	#i = 0;
	readonly #key: (value: V) => K;
	readonly #compare: (a: K, b: K) => number;

	constructor(key: (value: V) => K, compare: (a: K, b: K) => number) {
		this.#key = key;
		this.#compare = compare;
	}

	/** How many values it holds. */
	get size(): number {
		return this.#size;
	}

	/** The value with the lowest key. */
	first(): V | undefined {
		return this.#chunks[0]?.[0];
	}

	/** The value with the highest key. */
	last(): V | undefined {
		return this.#chunks.at(-1)?.at(-1);
	}

	/** The value with the lowest key at or above `key`. */
	ceil(key: K): V | undefined {
		this.#seek(key, true);
		return this.#at();
	}

	/** The value with the lowest key above `key`. */
	higher(key: K): V | undefined {
		this.#seek(key, false);
		return this.#at();
	}

	/** The value with the highest key at or below `key`. */
	floor(key: K): V | undefined {
		this.#seek(key, false);
		return this.#before();
	}

	/** The value with the highest key below `key`. */
	lower(key: K): V | undefined {
		this.#seek(key, true);
		return this.#before();
	}

	/** Every value with a key at or above `key`, in order. */
	*from(key: K): Generator<V> {
		this.#seek(key, true);
		let i = this.#i;
		for (let c = this.#c; c < this.#chunks.length; c++, i = 0) {
			const chunk = this.#chunks[c];
			for (; i < chunk.length; i++) yield chunk[i];
		}
	}

	/** Add `value`, whose key no stored value has. */
	insert(value: V): void {
		this.#size++;
		const last = this.#chunks.length - 1;
		if (last < 0) {
			this.#chunks.push([value]);
			return;
		}
		this.#seek(this.#key(value), false);
		let c = this.#c;
		let i = this.#i;
		if (c > last) {
			c = last;
			i = this.#chunks[last].length;
		}
		const chunk = this.#chunks[c];
		chunk.splice(i, 0, value);
		if (chunk.length > 2 * LOAD) this.#chunks.splice(c + 1, 0, chunk.splice(LOAD));
	}

	/** Remove the value stored under `key`, if any. */
	delete(key: K): void {
		this.#seek(key, true);
		const c = this.#c;
		const i = this.#i;
		const chunk = this.#chunks[c];
		if (!chunk || this.#compare(this.#key(chunk[i]), key) !== 0) return;
		this.#size--;
		chunk.splice(i, 1);
		if (this.#chunks.length === 1) {
			if (chunk.length === 0) this.#chunks.pop();
		} else if (chunk.length < LOAD / 2) {
			// Merge into a neighbor so chunks stay large enough to keep the chunk index short.
			const a = c > 0 ? c - 1 : c;
			const merged = this.#chunks[a].concat(this.#chunks[a + 1]);
			const half = merged.length >>> 1;
			if (merged.length > 2 * LOAD) this.#chunks.splice(a, 2, merged.slice(0, half), merged.slice(half));
			else this.#chunks.splice(a, 2, merged);
		}
	}

	// Point `#c` and `#i` at the first value whose key is above `key`, or at or above it when
	// `inclusive`. Fields rather than a returned pair, so a lookup allocates nothing.
	#seek(key: K, inclusive: boolean): void {
		let lo = 0;
		let hi = this.#chunks.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			const chunk = this.#chunks[mid];
			if (this.#above(chunk[chunk.length - 1], key, inclusive)) hi = mid;
			else lo = mid + 1;
		}
		this.#c = lo;
		const chunk = this.#chunks[lo];
		lo = 0;
		hi = chunk?.length ?? 0;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#above(chunk[mid], key, inclusive)) hi = mid;
			else lo = mid + 1;
		}
		this.#i = lo;
	}

	#above(value: V, key: K, inclusive: boolean): boolean {
		const order = this.#compare(this.#key(value), key);
		return inclusive ? order >= 0 : order > 0;
	}

	#at(): V | undefined {
		return this.#chunks[this.#c]?.[this.#i];
	}

	#before(): V | undefined {
		return this.#i > 0 ? this.#chunks[this.#c][this.#i - 1] : this.#chunks[this.#c - 1]?.at(-1);
	}
}
