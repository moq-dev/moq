import { Signal } from "@moq/signals";
import type { Options } from "./announced";
import type { Route } from "./hop";
import type { Table } from "./origin";
import type * as Path from "./path";

/** A closeable announcement snapshot for tests and benchmarks. @internal */
export async function announcedView(origin: Pick<Table, "announced">, scope?: Path.Pattern, options?: Options) {
	const stream = origin.announced(scope, options);
	const value = new Signal<ReadonlyMap<Path.Valid, Route>>(new Map());
	const done = (async () => {
		for await (const event of stream)
			value.update((previous) => {
				const next = new Map(previous);
				if (event.kind === "end") next.delete(event.prefix);
				else next.set(event.prefix, event.route);
				return next;
			});
	})();
	await new Promise<void>((resolve) => setImmediate(resolve));
	return {
		peek: () => value.peek(),
		subscribe: value.subscribe.bind(value),
		close: async () => {
			stream.close();
			await done;
		},
	};
}
