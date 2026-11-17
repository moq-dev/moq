import * as z from "zod/mini";

/**
 * The newest moq-hang draft this package reads. A catalog whose root `version` is higher is
 * refused, and an entry whose `version` is higher is skipped.
 */
export const VERSION = 4;

/** A catalog needs a newer moq-hang draft than this reader implements. */
export class UnsupportedVersion extends Error {
	/** The catalog version that was read. */
	readonly version: number;

	constructor(version: number) {
		super(`unsupported catalog version ${version}: upgrade to read it`);
		this.name = "UnsupportedVersion";
		this.version = version;
	}
}

/** A `version` member: the oldest moq-hang draft that reads what carries it, absent while that is 3. */
export const VersionSchema = z.number().check(z.int(), z.nonnegative());
