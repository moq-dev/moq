/** Preserve binding fields before Playwright serializes a rejection as just its message and stack. */
export async function diagnose<T>(task: Promise<T>): Promise<T> {
	try {
		return await task;
	} catch (error) {
		const seen = new WeakSet<object>();
		const detail = JSON.stringify(error, (_key, value) => {
			if (value !== null && typeof value === "object") {
				if (seen.has(value)) return "[Circular]";
				seen.add(value);
			}
			if (value instanceof Error)
				return { ...value, name: value.name, message: value.message, stack: value.stack, cause: value.cause };
			return typeof value === "bigint" ? String(value) : value;
		});
		throw new Error(detail ?? String(error));
	}
}
