// Renders the kramdown-rfc Internet-Drafts under drafts/ into VitePress pages.
//
// The draft sources stay canonical: `just drafts build` still feeds them to
// kramdown-rfc for the authoritative RFC rendering. This is the doc-site
// approximation, generated at config load so the pages exist before VitePress
// scans for routes.
//
// kramdown-rfc markdown is not CommonMark, and several of its constructs are
// actively hostile to VitePress:
//   - The metadata block runs to `--- abstract`, so gray-matter never finds
//     the closing fence and the whole document parses as frontmatter.
//   - `{{ref}}` citations collide with Vue template interpolation.
//   - `{::boilerplate}` / `{:key="value"}` IALs have no CommonMark equivalent.
//   - Tables use delimiter rows where GFM allows exactly one, so they render
//     as paragraphs.
// Everything below exists to translate those, one construct at a time.
//
// drafts.test.ts renders the output through VitePress itself, since the
// failure mode here is markdown that reads fine but renders as something else.

import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseYaml } from "yaml";

const here = dirname(fileURLToPath(import.meta.url));

/** kramdown-rfc sources. */
const SRC_DIR = join(here, "../../drafts");

/** Generated pages. Gitignored: regenerated on every build. */
const OUT_DIR = join(here, "../draft");

/** Route prefix for the generated pages. */
const BASE = "/draft";

/** Non-RFC, non-I-D bibxml references we cite, mapped to a canonical URL. */
const EXTERNAL: Record<string, { label: string; url: string }> = {
	WebCodecs: { label: "WebCodecs", url: "https://www.w3.org/TR/webcodecs/" },
};

/** What `{::boilerplate bcp14-tagged}` expands to. */
const BCP14 =
	'The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", ' +
	'"RECOMMENDED", "NOT RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as ' +
	"described in BCP 14 [RFC2119](https://www.rfc-editor.org/rfc/rfc2119) " +
	"[RFC8174](https://www.rfc-editor.org/rfc/rfc8174) when, and only when, they appear in all capitals, as shown here.";

/** A generated draft page. */
export interface DraftPage {
	/** Source draft name, e.g. `draft-lcurley-moq-lite`. */
	name: string;
	/** Draft title from the source metadata. */
	title: string;
	/** Site route, e.g. `/draft/moq-lite`. */
	link: string;
	/** The generated VitePress markdown. */
	markdown: string;
}

/** The metadata block above `--- abstract`. Only the fields we render. */
interface Metadata {
	title: string;
	docname: string;
	normative: Record<string, string>;
	informative: Record<string, string>;
	/** Nested (hand-written) reference definitions: alias -> title/target. */
	inline: Record<string, Reference>;
}

/** A citation target resolved to display text and (usually) a link. */
interface Reference {
	label: string;
	url?: string;
}

/** Translate every draft into a page, without touching disk. */
export function renderDrafts(): DraftPage[] {
	const names = readdirSync(SRC_DIR)
		.filter((f) => f.startsWith("draft-") && f.endsWith(".md"))
		.map((f) => f.slice(0, -3))
		.sort();

	// Cross-draft citations resolve to a local route rather than the
	// datatracker, so `{{moql}}` in one draft links to our page for another.
	const routes = new Map(names.map((name) => [name, `${BASE}/${slug(name)}`]));

	return names.map((name) => {
		const source = readFileSync(join(SRC_DIR, `${name}.md`), "utf8");
		const { meta, body } = render(source, routes);
		return { name, title: meta.title, link: `${BASE}/${slug(name)}`, markdown: body };
	});
}

/**
 * Regenerate the draft pages on disk and return them in sidebar order.
 *
 * Called from `config.ts` at module load, so the output is there before
 * VitePress enumerates routes. Editing a draft needs a dev-server restart.
 */
export function syncDrafts(): DraftPage[] {
	const pages = renderDrafts();

	rmSync(OUT_DIR, { recursive: true, force: true });
	mkdirSync(OUT_DIR, { recursive: true });

	for (const page of pages) {
		writeFileSync(join(OUT_DIR, `${slug(page.name)}.md`), page.markdown);
	}
	writeFileSync(join(OUT_DIR, "index.md"), renderIndex(pages));

	return pages;
}

/**
 * `draft-lcurley-moq-lite` -> `moq-lite`, the route segment.
 *
 * A draft from another author keeps its full name, so the mapping stays
 * reversible. `editLink` in config.ts inverts this.
 */
function slug(name: string): string {
	const prefix = "draft-lcurley-";
	return name.startsWith(prefix) ? name.slice(prefix.length) : name;
}

/** `draft-lcurley-moq-lite-latest` -> the datatracker page for the draft. */
function datatracker(docname: string): string {
	return `https://datatracker.ietf.org/doc/${docname.replace(/-latest$/, "")}/`;
}

/** Split a draft into its metadata block and the abstract/middle/back sections. */
function split(source: string): { meta: Metadata; abstract: string[]; middle: string[]; back: string[] } {
	const lines = source.split("\n");

	// The metadata block opens with `---` and closes at the first `--- <section>`.
	const start = lines.findIndex((l) => l.trim() === "---");
	const sections = new Map<string, number>();
	for (let i = start + 1; i < lines.length; i++) {
		const match = /^---\s+(abstract|middle|back)\s*$/.exec(lines[i]);
		if (match) sections.set(match[1], i);
	}

	const abstract = sections.get("abstract");
	const middle = sections.get("middle");
	const back = sections.get("back");
	if (abstract === undefined || middle === undefined || back === undefined) {
		throw new Error("draft is missing an --- abstract/middle/back section marker");
	}

	const parsed = parseYaml(lines.slice(start + 1, abstract).join("\n")) ?? {};
	const meta: Metadata = {
		title: String(parsed.title ?? "Untitled"),
		docname: String(parsed.docname ?? ""),
		normative: refs(parsed.normative),
		informative: refs(parsed.informative),
		inline: { ...inlineRefs(parsed.normative), ...inlineRefs(parsed.informative) },
	};

	return {
		meta,
		abstract: lines.slice(abstract + 1, middle),
		middle: lines.slice(middle + 1, back),
		back: lines.slice(back + 1),
	};
}

/**
 * Normalize a `normative:`/`informative:` block into alias -> target.
 *
 * A bare `RFC9000:` key parses as a null value and is its own target. Inline
 * reference definitions (a nested map of bibxml fields) are keyed by alias too,
 * so the alias doubles as the target when we can't do better.
 */
function refs(block: unknown): Record<string, string> {
	if (!block || typeof block !== "object") return {};
	const out: Record<string, string> = {};
	for (const [alias, target] of Object.entries(block as Record<string, unknown>)) {
		out[alias] = typeof target === "string" ? target : alias;
	}
	return out;
}

/**
 * Collect the nested reference definitions from a `normative:`/`informative:`
 * block, keeping their `title` and `target` so the citation renders as a real
 * link rather than falling back to the bare alias.
 */
function inlineRefs(block: unknown): Record<string, Reference> {
	if (!block || typeof block !== "object") return {};
	const out: Record<string, Reference> = {};
	for (const [alias, def] of Object.entries(block as Record<string, unknown>)) {
		if (!def || typeof def !== "object") continue;
		const { title, target } = def as { title?: unknown; target?: unknown };
		out[alias] = {
			label: typeof title === "string" ? title : alias,
			url: typeof target === "string" ? target : undefined,
		};
	}
	return out;
}

/** Resolve a citation target to display text and a URL. */
function resolve(target: string, routes: Map<string, string>, inline?: Record<string, Reference>): Reference {
	const rfc = /^RFC\s?(\d+)$/i.exec(target);
	if (rfc) {
		return { label: `RFC ${rfc[1]}`, url: `https://www.rfc-editor.org/rfc/rfc${rfc[1]}` };
	}

	const draft = /^I-D\.(.+)$/.exec(target);
	if (draft) {
		const docname = `draft-${draft[1]}`;
		return { label: docname, url: routes.get(docname) ?? datatracker(docname) };
	}

	return inline?.[target] ?? EXTERNAL[target] ?? { label: target };
}

/** Turn a resolved reference into a markdown link (or plain text if we have no URL). */
function link(ref: Reference, suffix?: string): string {
	const label = suffix ? `${ref.label}, ${suffix}` : ref.label;
	if (!ref.url) return label;

	// RFC section subreferences have a stable anchor; drafts don't.
	const section = suffix && ref.url.includes("rfc-editor.org") ? /^Section\s+([\d.]+)$/.exec(suffix) : null;
	return `[${label}](${ref.url}${section ? `#section-${section[1]}` : ""})`;
}

/** Collect `{#anchor}` -> heading text so internal citations can use the real title. */
function anchors(lines: string[]): Map<string, string> {
	const found = new Map<string, string>();
	for (let i = 0; i < lines.length; i++) {
		const heading = /^#{1,6}\s+(.*?)(?:\s+\{#([\w-]+)\})?\s*$/.exec(lines[i]);
		if (!heading) continue;

		// The anchor is either inline or on a `{: #anchor}` line below.
		const below = /^\{:\s*#([\w-]+)\}\s*$/.exec(lines[i + 1] ?? "");
		const anchor = heading[2] ?? below?.[1];
		if (anchor) found.set(anchor, heading[1]);
	}
	return found;
}

/** Is this a table delimiter row, like `|---|:--:|`? */
function isDelimiter(line: string): boolean {
	return /^\s*\|[-:|\s]+\|?\s*$/.test(line.trim()) && line.includes("-");
}

/** Is this any row of a table? */
function isRow(line: string): boolean {
	return line.trim().startsWith("|");
}

/**
 * Rewrite one table from kramdown's shape into GFM's.
 *
 * kramdown treats delimiter rows as decoration: one above the first row marks
 * it as a header, and more between body rows draw separators. GFM instead
 * gives exactly one meaning to exactly one delimiter, the alignment row
 * directly below the header, and renders nothing as a table without it. So
 * keep that one and drop every other delimiter.
 */
function table(rows: string[]): string[] {
	const body = rows[0] !== undefined && isDelimiter(rows[0]) ? rows.slice(1) : rows;
	const [header, ...rest] = body;
	if (header === undefined) return [];

	// Alignment (`|---:|:---|`) only survives if the row below the header
	// already carries it. Otherwise synthesize a plain one, since a header
	// with no delimiter at all is not a table.
	const columns = header
		.trim()
		.replace(/^\||\|$/g, "")
		.split("|").length;
	const [next] = rest;
	const alignment = next !== undefined && isDelimiter(next) ? next : `|${"---|".repeat(columns)}`;

	return [header, alignment, ...rest.filter((row) => !isDelimiter(row))];
}

/**
 * Rewrite the citations in one run of prose.
 *
 * kramdown-rfc has two forms: `{{ref}}`, which also happens to be Vue's
 * interpolation syntax, and `[ref]`, a link reference whose definition
 * kramdown-rfc generates from the metadata block. Both render as dangling
 * text (or worse) without a rewrite.
 */
function citeSegment(
	segment: string,
	meta: Metadata,
	labels: Map<string, string>,
	routes: Map<string, string>,
): string {
	const braces = segment.replace(/\{\{([^}]+)\}\}/g, (_, raw: string) => {
		// A leading sigil marks the reference normative/informative inline.
		const [target, ...rest] = raw.replace(/^[!?=<&]+/, "").split(",");
		const suffix = rest.join(",").trim() || undefined;
		const name = target.trim();

		const heading = labels.get(name);
		if (heading) return `[${heading}](#${name})`;

		const alias = meta.normative[name] ?? meta.informative[name];
		return link(resolve(alias ?? name, routes, meta.inline), suffix);
	});

	// `[ref]`, but not an image or an inline link. Only rewrite aliases the
	// draft actually declared, so ordinary brackets survive. A trailing colon
	// is left in place: a citation often just precedes one ("exactly as in
	// [qmux]: an endpoint advertises...").
	return braces.replace(/(!?)\[([^\][]+)\](?![([])/g, (whole, bang: string, name: string) => {
		if (bang) return whole;
		const alias = meta.normative[name] ?? meta.informative[name];
		return alias ? link(resolve(alias, routes, meta.inline)) : whole;
	});
}

/** Render one draft to a VitePress page. */
function render(source: string, routes: Map<string, string>): { meta: Metadata; body: string } {
	const { meta, abstract, middle, back } = split(source);
	const labels = anchors([...middle, ...back]);

	const cite = (line: string) => {
		// An actual link definition (`[ref]: url`) defines the alias rather than
		// citing it. The drafts have none today, since kramdown-rfc synthesizes
		// them, but rewriting one would corrupt it.
		if (/^\s*\[[^\][]+\]:/.test(line)) return line;

		// Inline code is verbatim, so only rewrite the segments between backticks.
		return line
			.split(/(`[^`]*`)/)
			.map((segment) => (segment.startsWith("`") ? segment : citeSegment(segment, meta, labels, routes)))
			.join("");
	};

	const body = (lines: string[]): string[] => {
		const out: string[] = [];
		let fenced = false;

		for (let i = 0; i < lines.length; i++) {
			const line = lines[i];

			if (/^(~~~|```)/.test(line)) {
				fenced = !fenced;
				// kramdown accepts bare `~~~`; keep fences uniform for shiki.
				out.push(line.startsWith("~~~") ? line.replace(/^~~~/, "```") : line);
				continue;
			}
			if (fenced) {
				out.push(line);
				continue;
			}

			if (/^\{::boilerplate\s+bcp14/.test(line.trim())) {
				out.push(BCP14);
				continue;
			}

			// `{: #anchor}` binds to the heading above; VitePress spells it inline.
			const anchor = /^\{:\s*#([\w-]+)\}\s*$/.exec(line.trim());
			if (anchor) {
				const prev = out.length - 1;
				if (prev >= 0 && /^#{1,6}\s/.test(out[prev])) out[prev] = `${out[prev]} {#${anchor[1]}}`;
				continue;
			}

			// Any other IAL (`{:numbered="false"}`) has no CommonMark meaning.
			if (/^\{:.*\}$/.test(line.trim())) continue;

			// Tables have to be reshaped as a whole, not row by row.
			if (isRow(line)) {
				let end = i;
				while (end < lines.length && isRow(lines[end])) end++;
				out.push(...table(lines.slice(i, end)).map(cite));
				i = end - 1;
				continue;
			}

			// Demote headings so the page title stays the only h1 and the
			// outline picks up the draft's own sections.
			out.push(cite(line.replace(/^(#{1,5})\s/, "#$1 ")));
		}
		return out;
	};

	const page = [
		"---",
		`title: ${JSON.stringify(meta.title)}`,
		"outline: deep",
		"---",
		"",
		`# ${meta.title}`,
		"",
		"::: info",
		"Rendered from the Internet-Draft source in this repository.",
		`Submitted versions are on the [IETF datatracker](${datatracker(meta.docname)}).`,
		":::",
		"",
		"## Abstract",
		...body(abstract),
		...body(middle),
		...references(meta, routes),
		...body(back),
		"",
	];

	return { meta, body: page.join("\n") };
}

/** The reference list kramdown-rfc would generate from the metadata block. */
function references(meta: Metadata, routes: Map<string, string>): string[] {
	const section = (heading: string, block: Record<string, string>): string[] => {
		const entries = Object.values(block);
		if (entries.length === 0) return [];
		return ["", `## ${heading}`, "", ...entries.map((target) => `- ${link(resolve(target, routes, meta.inline))}`)];
	};

	return [...section("Normative References", meta.normative), ...section("Informative References", meta.informative)];
}

/** The `/draft/` landing page. */
function renderIndex(pages: DraftPage[]): string {
	return [
		"---",
		'title: "Drafts"',
		// Generated from the whole directory, so there's no single source to edit.
		"editLink: false",
		"---",
		"",
		"# Internet-Drafts",
		"",
		"The IETF Internet-Drafts for the protocols implemented in this repository.",
		"These pages are generated from the [kramdown-rfc](https://github.com/cabo/kramdown-rfc) sources under `drafts/`;",
		"the submitted versions live on the [IETF datatracker](https://datatracker.ietf.org/person/kixelated@gmail.com).",
		"",
		...pages.map((page) => `- [${page.title}](${page.link}) (\`${page.name}\`)`),
		"",
	].join("\n");
}
