/** Compare numbers highest first, with missing values last. @internal */
export function compareNumber(left: number | undefined, right: number | undefined): number {
	if (left === right) return 0;
	if (left === undefined) return 1;
	if (right === undefined) return -1;
	return left > right ? -1 : 1;
}

/** Compare names by Unicode code point, matching Rust's string order. @internal */
export function compareName(left: string, right: string): number {
	// JS `<` compares UTF-16 code units and disagrees with Rust outside the BMP.
	const l = left[Symbol.iterator]();
	const r = right[Symbol.iterator]();
	for (;;) {
		const a = l.next();
		const b = r.next();
		if (a.done && b.done) return 0;
		if (a.done) return -1;
		if (b.done) return 1;
		const ac = a.value.codePointAt(0) ?? 0;
		const bc = b.value.codePointAt(0) ?? 0;
		if (ac !== bc) return ac < bc ? -1 : 1;
	}
}
