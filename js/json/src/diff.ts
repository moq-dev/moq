// RFC 7396 JSON Merge Patch: generate a patch between two values, and apply one.

export interface Diff {
	// A merge patch transforming the old value into the new one.
	patch: unknown;

	// Set when the change can't be faithfully expressed as a merge patch, so the caller should
	// publish a full snapshot instead. This happens when a value is set to null, which merge
	// patch reads as a key deletion. Arrays are fine: merge patch replaces them wholesale, which
	// is still typically smaller than a full snapshot.
	forcedSnapshot: boolean;
}

type Obj = Record<string, unknown>;

function isObject(value: unknown): value is Obj {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Generate an RFC 7396 merge patch transforming `oldVal` into `newVal`.
 *
 * Only object roots produce a recursive patch; any other root forces a snapshot.
 */
export function diff(oldVal: unknown, newVal: unknown): Diff {
	if (isObject(oldVal) && isObject(newVal)) {
		const patch: Obj = {};
		const forced = { value: false };
		diffObjects(oldVal, newVal, patch, forced);
		return { patch, forcedSnapshot: forced.value };
	}
	return { patch: newVal, forcedSnapshot: true };
}

function diffObjects(oldObj: Obj, newObj: Obj, patch: Obj, forced: { value: boolean }): void {
	// Keys present in old but missing from new become explicit null deletions.
	for (const key of Object.keys(oldObj)) {
		if (!(key in newObj)) patch[key] = null;
	}

	for (const key of Object.keys(newObj)) {
		const newV = newObj[key];
		const oldV = oldObj[key];
		const inOld = key in oldObj;

		if (inOld && deepEqual(oldV, newV)) continue;

		// Recurse into nested objects so unchanged sibling keys stay out of the patch.
		if (isObject(oldV) && isObject(newV)) {
			const sub: Obj = {};
			diffObjects(oldV, newV, sub, forced);
			if (Object.keys(sub).length > 0) patch[key] = sub;
			continue;
		}

		// Added or replaced with a non-object value. A literal null can't be stored: merge patch
		// would delete the key. Arrays are kept in the patch and replace the target wholesale.
		if (newV === null) {
			forced.value = true;
		}
		patch[key] = newV;
	}
}

/**
 * Apply an RFC 7396 merge patch to a target value, returning the result.
 *
 * Objects merge recursively; a null patch value deletes that key; any other patch value
 * (scalar or array) replaces the target wholesale.
 */
export function merge(target: unknown, patch: unknown): unknown {
	if (!isObject(patch)) return patch;

	const base: Obj = isObject(target) ? { ...target } : {};
	for (const [key, value] of Object.entries(patch)) {
		if (value === null) {
			delete base[key];
		} else {
			base[key] = merge(base[key], value);
		}
	}
	return base;
}

/** Structural equality for JSON values. */
export function deepEqual(a: unknown, b: unknown): boolean {
	if (a === b) return true;

	if (Array.isArray(a) && Array.isArray(b)) {
		return a.length === b.length && a.every((item, i) => deepEqual(item, b[i]));
	}

	if (isObject(a) && isObject(b)) {
		const keys = Object.keys(a);
		if (keys.length !== Object.keys(b).length) return false;
		return keys.every((key) => key in b && deepEqual(a[key], b[key]));
	}

	return false;
}
