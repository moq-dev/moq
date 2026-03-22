import type { z } from "zod";

/// A section definition that pairs a JSON key name with a Zod schema.
///
/// Used to register interest in specific catalog sections for reading or writing.
/// Audio and video sections are predefined but not registered by default.
export class Section<T> {
	readonly name: string;
	readonly schema: z.ZodType<T>;

	constructor(name: string, schema: z.ZodType<T>) {
		this.name = name;
		this.schema = schema;
	}
}
