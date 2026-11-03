/**
 * Counts JS-visible typed-array copies and allocations in a page.
 *
 * Installed as an init script, before any page module runs, and only in the counted pass:
 * wrapping these built-ins slows every caller, so its samples never feed a timing.
 * Native copies inside the browser, such as WebTransport filling a chunk, are invisible here.
 */
export function install(): void {
	const counts = { copied: 0, allocated: 0 };
	Object.defineProperty(globalThis, "__copies", { value: counts });

	const typed = Object.getPrototypeOf(Uint8Array.prototype) as {
		set(this: Uint8Array, source: ArrayLike<number>, offset?: number): void;
		slice(this: Uint8Array, start?: number, end?: number): Uint8Array;
	};
	const set = typed.set;
	typed.set = function (source, offset) {
		counts.copied += source.length * this.BYTES_PER_ELEMENT;
		return set.call(this, source, offset);
	};
	const slice = typed.slice;
	typed.slice = function (start, end) {
		const result = slice.call(this, start, end);
		counts.copied += result.byteLength;
		counts.allocated += result.byteLength;
		return result;
	};
	const bufferSlice = ArrayBuffer.prototype.slice;
	ArrayBuffer.prototype.slice = function (start?: number, end?: number) {
		const result = bufferSlice.apply(this, [start, end] as [number, number]);
		counts.copied += result.byteLength;
		counts.allocated += result.byteLength;
		return result;
	};

	// A length allocates, a buffer is a view, and anything else is copied into new storage.
	const wrap = <T extends abstract new (...args: never[]) => object>(name: string, native: T) => {
		Object.defineProperty(globalThis, name, {
			configurable: true,
			writable: true,
			value: new Proxy(native, {
				construct(target, args, newTarget) {
					const result = Reflect.construct(target, args, newTarget) as { byteLength: number };
					const [first] = args as unknown[];
					if (typeof first === "number" || first === undefined) counts.allocated += result.byteLength;
					// SharedArrayBuffer only exists in cross-origin isolated pages.
					else if (
						!(first instanceof ArrayBuffer) &&
						!(typeof SharedArrayBuffer !== "undefined" && first instanceof SharedArrayBuffer)
					) {
						counts.copied += result.byteLength;
						counts.allocated += result.byteLength;
					}
					return result;
				},
			}),
		});
	};
	wrap("Uint8Array", Uint8Array);
	wrap("ArrayBuffer", ArrayBuffer);
}

/** Reads the counters `install` left on the page. */
export function read(): { copied: number; allocated: number } {
	const counts = (globalThis as { __copies?: { copied: number; allocated: number } }).__copies;
	if (!counts) throw new Error("copy counters are not installed");
	return { ...counts };
}
