import { expect, test } from "bun:test";
import { Effect } from "@moq/signals";
import { SupportProbe } from "./support-probe";

test("a supported rendition named __proto__ is published as its own ID", async () => {
	const probe = new SupportProbe<{ codec: string }>("video");
	const effect = new Effect();
	// JSON.parse keeps `__proto__` as an own key, as the catalog parser does.
	const renditions = JSON.parse('{"__proto__":{"codec":"avc1"},"hd":{"codec":"avc1"}}') as Record<
		string,
		{ codec: string }
	>;
	const published = Promise.withResolvers<Record<string, { codec: string }>>();
	probe.run(effect, async () => true, renditions, published.resolve);
	const available = await published.promise;
	expect(Object.keys(available).sort()).toEqual(["__proto__", "hd"]);
	expect(Object.hasOwn(available, "__proto__")).toBe(true);
	effect.close();
});
