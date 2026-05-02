import { Path } from "@moq/lite";

/**
 * Resolve a relative broadcast reference against the path of the broadcast that served the catalog.
 *
 * `..` segments pop the last segment of the base path; named segments are appended.
 * Excess `..` clamps at the root, returning an empty path. An empty `rel` returns the base
 * path unchanged.
 *
 * Mirrors the Rust `Path::resolve(&PathRelative)` helper used by hang catalogs to express
 * cross-broadcast track references.
 *
 * @example
 * ```typescript
 * resolveBroadcast(Path.from("a/b/c"), "../source"); // "a/b/source"
 * resolveBroadcast(Path.from("a/b"), "x/y");          // "a/b/x/y"
 * resolveBroadcast(Path.from("a"), "../../x");        // "x"
 * ```
 */
export function resolveBroadcast(base: Path.Valid, rel: string): Path.Valid {
	const baseSegments = base === "" ? [] : base.split("/");
	const relSegments = rel.split("/").filter((s) => s !== "");

	for (const seg of relSegments) {
		if (seg === "..") {
			baseSegments.pop();
		} else {
			baseSegments.push(seg);
		}
	}

	return Path.from(...baseSegments);
}
