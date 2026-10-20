/**
 * Path grants and optional per-connection bitrate caps.
 * @module
 */
import { Pattern, Patterns } from "@moq/pattern";
import * as z from "@zod/mini";

/** A list of pattern texts, each validated by `Pattern.parse`. */
const PatternSchema = z.string().check(
	z.refine(
		(text) => {
			try {
				Pattern.parse(text);
				return true;
			} catch {
				return false;
			}
		},
		{ message: "invalid path pattern" },
	),
);

/** A list of validated pattern texts. */
export const PatternListSchema = z.array(PatternSchema);

/** Parse a list of pattern texts into a reduced union. */
export function patterns(texts: readonly string[] | undefined): Patterns {
	return new Patterns((texts ?? []).map((text) => Pattern.parse(text)));
}

const RateSchema = z.int().check(z.positive());
const PathsSchema = z.union([PatternSchema, PatternListSchema, z.instanceof(Patterns)]);

/** Paths a role may access and its optional per-connection bitrate cap. */
export class Access {
	/** Patterns this role may access, relative to its root. */
	readonly path: Patterns;
	/** Maximum bits per second for this direction, or no cap when absent. */
	readonly rate?: number;

	/** Build access from paths and an optional positive bitrate in bits per second. */
	constructor(config: { path: Patterns; rate?: number }) {
		this.path = new Patterns(config.path);
		this.rate = config.rate === undefined ? undefined : RateSchema.parse(config.rate);
	}

	/** Encode uncapped paths compactly and keep every cap explicit. */
	toJSON(): string[] | { path: string[]; rate: number } {
		const path = this.path.toJSON();
		return this.rate === undefined ? path : { path, rate: this.rate };
	}
}

/** @internal */
type AccessInput = string | string[] | Patterns | { path: string | string[] | Patterns; rate?: number };

/** @internal */
export function normalizeAccess(value: AccessInput): Access {
	const paths = (value: string | string[] | Patterns) =>
		value instanceof Patterns ? value : patterns(typeof value === "string" ? [value] : value);
	if (typeof value === "string" || Array.isArray(value) || value instanceof Patterns) {
		return new Access({ path: paths(value) });
	}
	return new Access({ path: paths(value.path), rate: value.rate });
}

/** Validate string, list, or object access without losing its bitrate cap. */
export const AccessSchema = z.pipe(
	z.union([PathsSchema, z.strictObject({ path: PathsSchema, rate: z.optional(RateSchema) })]),
	z.transform(normalizeAccess),
);

/** @internal */
export const ScopeAccessSchema = z.pipe(
	z.union([PathsSchema, z.strictObject({ path: PathsSchema })]),
	z.transform((value) => normalizeAccess(value).path.toJSON()),
);
