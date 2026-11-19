/**
 * Counted runs of group keys, so one edit and each query cost O(log n) however many runs are held.
 * Mirrors the Rust `Cover`, without its persistence.
 *
 * A subscription may name thousands of ranges, and an update may edit one of them. Each run counts
 * +1 at its start and -1 at its exclusive end, so a key is covered while the running sum through it
 * stays positive. A treap keeps each subtree's sum and its lowest and highest running sums, so
 * adding or removing a run touches two keys and every query is one descent.
 *
 * @module
 */

import { min, UNBOUNDED } from "./util/key.ts";

/** A run of keys `[start, end)`; an open run ends at {@link UNBOUNDED}. */
type Run = { start: bigint; end: bigint };

interface Node<K> {
	key: K;
	// Runs starting here, minus runs ending here. Never zero.
	delta: number;
	// Random, so no choice of keys can unbalance the tree.
	priority: number;
	// The sum of every delta in this subtree.
	sum: number;
	// The lowest and highest running sums at any key of this subtree, counted from its first.
	low: number;
	high: number;
	left?: Node<K>;
	right?: Node<K>;
}

type Link<K> = Node<K> | undefined;

function sum<K>(node: Link<K>): number {
	return node ? node.sum : 0;
}

function low<K>(node: Link<K>): number {
	return node ? node.low : Number.POSITIVE_INFINITY;
}

function high<K>(node: Link<K>): number {
	return node ? node.high : Number.NEGATIVE_INFINITY;
}

function pull<K>(node: Node<K>): void {
	const at = sum(node.left) + node.delta;
	node.sum = at + sum(node.right);
	node.low = Math.min(low(node.left), at, at + low(node.right));
	node.high = Math.max(high(node.left), at, at + high(node.right));
}

// Split into the keys below `key` (through it when `inclusive`) and the rest.
function split<K extends number | bigint>(node: Link<K>, key: K, inclusive: boolean): [Link<K>, Link<K>] {
	if (!node) return [undefined, undefined];
	if (node.key < key || (inclusive && node.key === key)) {
		const [below, rest] = split(node.right, key, inclusive);
		node.right = below;
		pull(node);
		return [node, rest];
	}
	const [rest, above] = split(node.left, key, inclusive);
	node.left = above;
	pull(node);
	return [rest, node];
}

// Join two treaps, every key of `left` below every key of `right`.
function merge<K>(left: Link<K>, right: Link<K>): Link<K> {
	if (!left) return right;
	if (!right) return left;
	if (left.priority > right.priority) {
		left.right = merge(left.right, right);
		pull(left);
		return left;
	}
	right.left = merge(left, right.left);
	pull(right);
	return right;
}

// Add `delta` at `key`, dropping its node once its delta reaches zero: one descent to find the
// key, then one to adjust or insert it.
function shift<K extends number | bigint>(root: Link<K>, key: K, delta: number): Link<K> {
	if (find(root, key)) return adjust(root as Node<K>, key, delta);
	return insert(root, { key, delta, priority: Math.random(), sum: delta, low: delta, high: delta });
}

// Add `delta` at `key`, which the subtree under `node` holds.
function adjust<K extends number | bigint>(node: Node<K>, key: K, delta: number): Link<K> {
	if (key === node.key) {
		node.delta += delta;
		if (node.delta === 0) return merge(node.left, node.right);
	} else if (key < node.key) node.left = adjust(node.left as Node<K>, key, delta);
	else node.right = adjust(node.right as Node<K>, key, delta);
	pull(node);
	return node;
}

// Insert `fresh`, whose key the subtree under `node` lacks, where its priority places it.
function insert<K extends number | bigint>(node: Link<K>, fresh: Node<K>): Node<K> {
	if (!node) return fresh;
	if (fresh.priority > node.priority) {
		[fresh.left, fresh.right] = split(node, fresh.key, false);
		pull(fresh);
		return fresh;
	}
	if (fresh.key < node.key) node.left = insert(node.left, fresh);
	else node.right = insert(node.right, fresh);
	pull(node);
	return node;
}

function find<K extends number | bigint>(node: Link<K>, key: K): Node<K> | undefined {
	while (node && node.key !== key) node = key < node.key ? node.left : node.right;
	return node;
}

// The lowest running sum at any key of the subtree in `(lo, hi)`, entered with `offset` runs open.
function lowIn(node: Link<bigint>, offset: number, lo: bigint, hi: bigint): number {
	if (!node) return Number.POSITIVE_INFINITY;
	const at = offset + sum(node.left) + node.delta;
	if (node.key <= lo) return lowIn(node.right, at, lo, hi);
	if (node.key >= hi) return lowIn(node.left, offset, lo, hi);
	return Math.min(lowAbove(node.left, offset, lo), at, lowBelow(node.right, at, hi));
}

// {@link lowIn} for a subtree whose every key is below `hi`.
function lowAbove(node: Link<bigint>, offset: number, lo: bigint): number {
	if (!node) return Number.POSITIVE_INFINITY;
	const at = offset + sum(node.left) + node.delta;
	if (node.key <= lo) return lowAbove(node.right, at, lo);
	return Math.min(lowAbove(node.left, offset, lo), at, at + low(node.right));
}

// {@link lowIn} for a subtree whose every key is above `lo`.
function lowBelow(node: Link<bigint>, offset: number, hi: bigint): number {
	if (!node) return Number.POSITIVE_INFINITY;
	const at = offset + sum(node.left) + node.delta;
	if (node.key >= hi) return lowBelow(node.left, offset, hi);
	return Math.min(offset + low(node.left), at, lowBelow(node.right, at, hi));
}

function first<K>(node: Link<K>): K | undefined {
	if (!node) return undefined;
	while (node.left) node = node.left;
	return node.key;
}

function last<K>(node: Link<K>): K | undefined {
	if (!node) return undefined;
	while (node.right) node = node.right;
	return node.key;
}

// The running sum through `key`.
function count<K extends number | bigint>(root: Link<K>, key: K): number {
	let total = 0;
	let node = root;
	while (node) {
		if (node.key <= key) {
			total += sum(node.left) + node.delta;
			node = node.right;
		} else node = node.left;
	}
	return total;
}

// Whether a running sum is what is searched for: covered (above zero), or a gap.
type Want = (sum: number) => boolean;
const COVERED: Want = (sum) => sum > 0;
const GAP: Want = (sum) => sum <= 0;

// Whether a subtree entered with `offset` may hold a running sum `want` accepts.
function within(offset: number, node: Node<bigint>, want: Want): boolean {
	return want === COVERED ? offset + node.high > 0 : offset + node.low <= 0;
}

// The first key of the subtree, entered with `offset` runs open, whose running sum is `want`.
function firstIn(node: Link<bigint>, offset: number, want: Want): bigint | undefined {
	if (!node || !within(offset, node, want)) return undefined;
	const left = firstIn(node.left, offset, want);
	if (left !== undefined) return left;
	const at = offset + sum(node.left) + node.delta;
	if (want(at)) return node.key;
	return firstIn(node.right, at, want);
}

// The last key of the subtree, entered with `offset` runs open, whose running sum is `want`.
function lastIn(node: Link<bigint>, offset: number, want: Want): bigint | undefined {
	if (!node || !within(offset, node, want)) return undefined;
	const at = offset + sum(node.left) + node.delta;
	const right = lastIn(node.right, at, want);
	if (right !== undefined) return right;
	if (want(at)) return node.key;
	return lastIn(node.left, offset, want);
}

// {@link firstIn} over the keys at or after `from`.
function firstFrom(node: Link<bigint>, offset: number, from: bigint, want: Want): bigint | undefined {
	if (!node) return undefined;
	const at = offset + sum(node.left) + node.delta;
	if (node.key < from) return firstFrom(node.right, at, from, want);
	const left = firstFrom(node.left, offset, from, want);
	if (left !== undefined) return left;
	if (want(at)) return node.key;
	return firstIn(node.right, at, want);
}

/** A multiset of runs of keys, read as the set of keys any run covers. */
export class Cover {
	#root: Link<bigint>;

	/** Count `delta` more of `[start, end)`, or fewer when negative; an empty run counts nothing. */
	add(start: bigint, end: bigint, delta: number): void {
		if (end <= start || delta === 0) return;
		this.#root = shift(this.#root, start, delta);
		if (end < UNBOUNDED) this.#root = shift(this.#root, end, -delta);
	}

	/** Whether no run is counted. */
	get empty(): boolean {
		return this.#root === undefined;
	}

	/** Whether any run covers `key`. */
	contains(key: bigint): boolean {
		return count(this.#root, key) > 0;
	}

	/** The fewest runs covering any key in `[start, end)`. */
	least(start: bigint, end: bigint): number {
		return Math.min(count(this.#root, start), lowIn(this.#root, 0, start, end));
	}

	/** The lowest key covered at or after `from`. */
	next(from: bigint): bigint | undefined {
		return this.contains(from) ? from : firstFrom(this.#root, 0, from, COVERED);
	}

	/** The lowest key covered: where the first run starts. */
	lowest(): bigint | undefined {
		return first(this.#root);
	}

	/** The exclusive end past every key covered: {@link UNBOUNDED} while a run is open, 0 for none. */
	end(): bigint {
		if (sum(this.#root) > 0) return UNBOUNDED;
		return last(this.#root) ?? 0n;
	}

	/** Where the run reaching past every key starts, undefined when the last run ends. */
	openFrom(): bigint | undefined {
		if (sum(this.#root) <= 0) return undefined;
		// Past the last gap edge, or from the first edge when every edge leaves a run open.
		const gap = lastIn(this.#root, 0, GAP);
		return gap === undefined ? this.lowest() : firstFrom(this.#root, 0, gap, COVERED);
	}

	/**
	 * The runs reaching into `[from, end)`, lowest first, clipped to it. One walk in key order that
	 * skips every subtree holding no run's edge, so each run costs about one descent.
	 */
	runs(from = 0n, end = UNBOUNDED): Run[] {
		const out: Run[] = [];
		if (from >= end) return out;
		// Every edge above `from`, each with how many runs were open entering it.
		const stack: [Node<bigint>, number][] = [];
		let open: bigint | undefined;
		let offset = 0;
		let node = this.#root;
		while (node) {
			if (node.key <= from) {
				offset += sum(node.left) + node.delta;
				node = node.right;
			} else {
				stack.push([node, offset]);
				node = node.left;
			}
		}
		if (offset > 0) open = from;
		// Queue the subtree under `link` down its left edge, leaving out each subtree whose running
		// sum stays on the side it entered on.
		const descend = (link: Link<bigint>, offset: number) => {
			while (link) {
				const crosses = offset > 0 ? offset + link.low <= 0 : offset + link.high > 0;
				if (!crosses) return;
				stack.push([link, offset]);
				link = link.left;
			}
		};
		for (let top = stack.pop(); top; top = stack.pop()) {
			const [node, offset] = top;
			if (open === undefined && node.key >= end) return out;
			const at = offset + sum(node.left) + node.delta;
			descend(node.right, at);
			if (open !== undefined && at <= 0) {
				out.push({ start: open, end: min(node.key, end) });
				open = undefined;
			} else if (open === undefined && at > 0) open = node.key;
		}
		if (open !== undefined && open < end) out.push({ start: open, end });
		return out;
	}
}

/** A multiset of keys, counted so that adding, removing, and finding the lowest or highest cost O(log n). */
export class Counts<K extends number | bigint> {
	#root: Link<K>;

	/** Count `key` once more, returning whether it is new. */
	add(key: K): boolean {
		const fresh = !this.has(key);
		this.#root = shift(this.#root, key, 1);
		return fresh;
	}

	/** Count `key` once less, returning whether that was its last. Nothing for a key not held. */
	remove(key: K): boolean {
		const held = this.#find(key)?.delta;
		if (held === undefined) return false;
		this.#root = shift(this.#root, key, -1);
		return held === 1;
	}

	/** Whether `key` is held. */
	has(key: K): boolean {
		return this.#find(key) !== undefined;
	}

	/** The lowest key held. */
	first(): K | undefined {
		return first(this.#root);
	}

	/** The highest key held. */
	last(): K | undefined {
		return last(this.#root);
	}

	/** The keys held at or below `limit`, lowest first, costing those keys and one descent. */
	through(limit: K): K[] {
		const keys: K[] = [];
		const walk = (node: Link<K>) => {
			if (!node) return;
			walk(node.left);
			if (node.key > limit) return;
			keys.push(node.key);
			walk(node.right);
		};
		walk(this.#root);
		return keys;
	}

	#find(key: K): Node<K> | undefined {
		return find(this.#root, key);
	}
}
