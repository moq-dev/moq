// Checks the generated pages against the real VitePress markdown pipeline.
//
// Asserting on the generated markdown alone is not enough: the failure mode
// these guard against is markdown that reads fine but renders as something
// else. A table missing its GFM delimiter row, for instance, silently
// degrades into a paragraph.

import { describe, expect, test } from "bun:test";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createMarkdownRenderer } from "vitepress";
import { type DraftPage, renderDrafts } from "./drafts";

const srcDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const md = await createMarkdownRenderer(srcDir);

const pages = renderDrafts();

/** Everything below the generated frontmatter. */
function content(page: DraftPage): string {
	return page.markdown.split("\n---\n").slice(1).join("\n---\n");
}

function html(page: DraftPage): string {
	return md.render(content(page));
}

test("every draft produces a page", () => {
	expect(pages.length).toBeGreaterThan(0);
	expect(pages.map((p) => p.name)).toContain("draft-lcurley-moq-lite");
});

describe("kramdown constructs are translated", () => {
	test.each(pages.map((p) => [p.name, p] as const))("%s", (_name, page) => {
		const body = content(page);

		// `{{ref}}` is Vue interpolation, so a survivor is a build hazard, not
		// just a typo. The rest would render as literal punctuation.
		expect(body).not.toInclude("{{");
		expect(body).not.toMatch(/^\{:/m);
		expect(body).not.toMatch(/^--- (abstract|middle|back)$/m);
		expect(body).not.toInclude("{::boilerplate");
	});
});

describe("citations resolve", () => {
	test.each(pages.map((p) => [p.name, p] as const))("%s", (_name, page) => {
		// A `[ref]` that never became a link renders as literal brackets. Match
		// the alias shape kramdown-rfc uses (letter-initial, so example output
		// like `sizes=[56]` doesn't count) and allow real link/image syntax.
		const dangling: string[] = [];
		let fenced = false;

		for (const line of content(page).split("\n")) {
			if (line.startsWith("```")) fenced = !fenced;
			if (fenced) continue;

			for (const [, bang, name] of line.matchAll(/(!?)\[([A-Za-z][\w.-]*)\](?![([])/g)) {
				if (!bang) dangling.push(name);
			}
		}

		expect(dangling).toEqual([]);
	});

	test("a citation before a colon is still a link", () => {
		const page = pages.find((p) => p.name === "draft-lcurley-qmux-websocket");
		expect(page).toBeDefined();

		// "...exactly as in [qmux]: an endpoint advertises..." is prose, not a
		// link definition, so the alias has to resolve.
		expect(content(page as DraftPage)).not.toInclude("[qmux]:");
		expect(html(page as DraftPage)).toInclude("draft-ietf-quic-qmux</a>: an endpoint");
	});
});

describe("tables render as tables", () => {
	test.each(pages.map((p) => [p.name, p] as const))("%s", (_name, page) => {
		// kramdown draws separators between body rows; GFM allows exactly one
		// delimiter, below the header. Every source table must survive.
		const source = content(page)
			.split("\n")
			.filter((line) => line.trim().startsWith("|"));
		if (source.length === 0) return;

		const rendered = html(page);
		expect(rendered.match(/<table/g) ?? []).toHaveLength(countTables(content(page)));

		// A dropped delimiter leaves the rows as paragraph text.
		expect(rendered).not.toMatch(/<p>\s*\|/);
	});

	test("row separators do not split a table apart", () => {
		const page = pages.find((p) => p.name === "draft-lcurley-moq-lite");
		expect(page).toBeDefined();

		const rendered = html(page as DraftPage);

		// The Bidirectional Streams table: one header, six body rows, each
		// separated by a kramdown rule in the source. Cells carry the source's
		// alignment, so match around the style attribute.
		const header = /<th[^>]*>Stream<\/th>/.exec(rendered);
		expect(header).not.toBeNull();

		for (const stream of ["Announce", "Subscribe", "Fetch", "Probe", "Goaway", "Track"]) {
			expect(rendered).toMatch(new RegExp(`<td[^>]*>${stream}</td>`));
		}

		// One header row plus six body rows, in a single table.
		const start = rendered.lastIndexOf("<table", header?.index);
		const rows = rendered.slice(start).split("</table>")[0];
		expect(rows.match(/<tr>/g) ?? []).toHaveLength(7);
	});
});

/** Count the table blocks in a page: runs of consecutive `|` lines. */
function countTables(body: string): number {
	const lines = body.split("\n");
	let count = 0;
	let fenced = false;

	for (let i = 0; i < lines.length; i++) {
		if (lines[i].startsWith("```")) fenced = !fenced;
		if (fenced) continue;

		const row = lines[i].trim().startsWith("|");
		const previous = i > 0 && lines[i - 1].trim().startsWith("|");
		if (row && !previous) count++;
	}
	return count;
}
