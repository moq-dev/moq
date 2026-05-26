import { Path } from "@moq/net";
import * as z from "zod/mini";

/**
 * Normalize a relative broadcast string the way Rust `PathRelative::new` does: trim
 * leading/trailing slashes, drop empty segments, and drop `.` segments. `..` is preserved
 * and only interpreted by `resolveBroadcast`.
 *
 * Returns the normalized form. Two callers comparing normalized strings can detect that
 * `""`, `"."`, `"/./"` etc. all mean "no override".
 */
export function normalizeRelativeBroadcast(rel: string): string {
	return rel
		.split("/")
		.filter((s) => s !== "" && s !== ".")
		.join("/");
}

/**
 * Resolve a relative broadcast reference against the path of the broadcast that served the catalog.
 *
 * `..` segments pop the last segment of the base path; other segments are appended.
 * `.` and empty segments are no-ops. Excess `..` once the base is empty is also a no-op
 * (subsequent named segments still append). An empty / normalized-empty `rel` returns the
 * base path unchanged.
 *
 * Mirrors the Rust `Path::resolve(&PathRelative)` helper used by hang catalogs to express
 * cross-broadcast track references.
 *
 * @example
 * ```typescript
 * resolveBroadcast(Path.from("a/b/c"), "../source"); // "a/b/source"
 * resolveBroadcast(Path.from("a/b"), "x/y");          // "a/b/x/y"
 * resolveBroadcast(Path.from("a"), "../../x");        // "x"
 * resolveBroadcast(Path.from("a/b"), "./c");          // "a/b/c"
 * ```
 */
/**
 * Zod schema for a relative broadcast reference stored in a catalog. Normalizes the input
 * the same way Rust `PathRelative::new` does so JS and Rust agree byte-for-byte on what's
 * stored in memory after deserialization.
 */
export const RelativeBroadcastSchema = z.pipe(z.string(), z.transform(normalizeRelativeBroadcast));

export function resolveBroadcast(base: Path.Valid, rel: string): Path.Valid {
	const baseSegments = base === "" ? [] : base.split("/").filter((s) => s !== "");
	const relSegments = rel.split("/").filter((s) => s !== "" && s !== ".");

	for (const seg of relSegments) {
		if (seg === "..") {
			baseSegments.pop();
		} else {
			baseSegments.push(seg);
		}
	}

	return Path.from(...baseSegments);
}
