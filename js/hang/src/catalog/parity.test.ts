import { expect, test } from "bun:test";
import { checkRenditions, RootSchema } from "./root.ts";

interface Case {
	name: string;
	catalog: unknown;
	refused?: boolean;
	video?: string[];
	audio?: string[];
	text?: string[];
	json?: string[];
	binary?: string[];
}

// The corpus rs/hang also loads: both languages accept the same catalogs and keep the same
// rendition and data track IDs, so a divergence fails CI on whichever side drifts.
const cases = (await Bun.file(
	new URL("../../../../rs/hang/fixtures/catalog-parity.json", import.meta.url),
).json()) as Case[];

// Rust's BTreeMap order, so the lists compare as recorded.
const ids = (renditions: Record<string, unknown> | undefined) => Object.keys(renditions ?? {}).sort();

for (const c of cases) {
	test(`shared parity fixture: ${c.name}`, () => {
		const parse = () => checkRenditions(RootSchema.parse(c.catalog));
		if (c.refused) {
			expect(parse).toThrow();
			return;
		}
		const root = parse();
		expect(ids(root.video?.renditions)).toEqual(c.video ?? []);
		expect(ids(root.audio?.renditions)).toEqual(c.audio ?? []);
		expect(ids(root.text?.renditions)).toEqual(c.text ?? []);
		expect(ids(root.json?.tracks)).toEqual(c.json ?? []);
		expect(ids(root.binary?.tracks)).toEqual(c.binary ?? []);
	});
}
