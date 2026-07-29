import { beforeAll, describe, expect, test } from "bun:test";
import { VTTCue } from "media-captions";
import { type CueStore, clearCue, insertCue, pruneCues, rollUp, sanitizeCue } from "./renderer";

function store(cues: VTTCue[] = []): CueStore {
	return { cues, regions: new Map(), clears: [] };
}

// Build a cue store by inserting in the given delivery order, returning the resulting start times.
function starts(order: number[]): number[] {
	const cues: VTTCue[] = [];
	for (const start of order) insertCue(cues, new VTTCue(start, start + 1, `cue ${start}`));
	return cues.map((cue) => cue.startTime);
}

describe("insertCue", () => {
	test("keeps cues sorted when they arrive in order", () => {
		expect(starts([0, 1, 2, 3])).toEqual([0, 1, 2, 3]);
	});

	// Groups are handed out in delivery order, not sequence order, so a cue can land before one
	// already stored. Appending blindly would render captions out of order.
	test("keeps cues sorted when they arrive out of order", () => {
		expect(starts([2, 0, 3, 1])).toEqual([0, 1, 2, 3]);
	});

	test("keeps equal start times in arrival order", () => {
		const cues: VTTCue[] = [];
		insertCue(cues, new VTTCue(1, 2, "first"));
		insertCue(cues, new VTTCue(1, 2, "second"));
		expect(cues.map((c) => c.text)).toEqual(["first", "second"]);
	});
});

describe("rollUp", () => {
	// A utf8 cue has no real end: it runs until the next one starts.
	test("ends the previous cue where this one begins", () => {
		const s = store();
		const first = new VTTCue(0, 30, "first");
		insertCue(s.cues, first);
		rollUp(s, first);

		const second = new VTTCue(2, 32, "second");
		insertCue(s.cues, second);
		rollUp(s, second);

		expect(first.endTime).toBe(2);
		expect(second.endTime).toBe(32);
	});

	// The successor may already have arrived, in which case the late cue is the one to clamp.
	test("clamps a late cue against the successor already stored", () => {
		const s = store();
		const later = new VTTCue(4, 34, "later");
		insertCue(s.cues, later);
		rollUp(s, later);

		const earlier = new VTTCue(1, 31, "earlier");
		insertCue(s.cues, earlier);
		rollUp(s, earlier);

		expect(earlier.endTime).toBe(4);
		expect(later.startTime).toBe(4);
		expect(later.endTime).toBe(34);
	});

	test("leaves a lone cue lingering", () => {
		const s = store();
		const only = new VTTCue(0, 30, "only");
		insertCue(s.cues, only);
		rollUp(s, only);
		expect(only.endTime).toBe(30);
	});
});

describe("clearCue", () => {
	// The hang draft defines an empty utf8 payload as clearing the caption, so it has to be
	// scheduled rather than dropped: otherwise stale accessibility text stays on screen.
	test("ends the cue showing at the clear time", () => {
		const s = store([new VTTCue(0, 30, "showing")]);
		clearCue(s, 5);
		expect(s.cues[0].endTime).toBe(5);
		expect(s.cues).toHaveLength(1);
	});

	test("ignores cues that have not started yet", () => {
		const s = store([new VTTCue(0, 2, "done"), new VTTCue(10, 40, "later")]);
		clearCue(s, 5);
		expect(s.cues[0].endTime).toBe(2);
		expect(s.cues[1].endTime).toBe(40);
	});

	test("is a no-op with nothing showing", () => {
		expect(() => clearCue(store(), 5)).not.toThrow();
	});

	// Groups are read concurrently, so a clear can complete before the cue it terminates. The clear
	// used to be a no-op then, leaving stale accessibility text up for the full 30s linger.
	test("still applies when it arrives before the cue it ends", () => {
		const s = store();
		clearCue(s, 4);

		const late = new VTTCue(1, 31, "late");
		insertCue(s.cues, late);
		rollUp(s, late);

		expect(late.endTime).toBe(4);
	});
});

describe("pruneCues", () => {
	test("drops expired cues and reports the change", () => {
		const s = store([new VTTCue(0, 1, "old"), new VTTCue(2, 3, "old"), new VTTCue(9, 10, "fresh")]);
		expect(pruneCues(s, 5)).toBe(true);
		expect(s.cues.map((c) => c.text)).toEqual(["fresh"]);
		expect(pruneCues(s, 5)).toBe(false);
	});

	// Cues are ordered by start time, not end time. A long-running early cue used to stop the
	// sweep at the head, letting every expired cue behind it accumulate for the whole stream.
	test("prunes behind a long-running early cue", () => {
		const s = store([new VTTCue(0, 1_000, "long")]);
		for (let i = 1; i <= 20; i++) s.cues.push(new VTTCue(i, i + 0.5, `short ${i}`));

		expect(pruneCues(s, 15)).toBe(true);
		expect(s.cues.map((c) => c.text)).toEqual([
			"long",
			"short 15",
			"short 16",
			"short 17",
			"short 18",
			"short 19",
			"short 20",
		]);
	});
});

describe("sanitizeCue", () => {
	// The renderer decodes HTML entities in cue text and then splices the result into innerHTML, so
	// an entity-encoded element in an untrusted payload becomes a live one. Assert against the
	// library's real render path rather than just this helper, since that is what ships.
	let render: (cue: VTTCue) => string;

	beforeAll(async () => {
		const url = new URL("../../../../node_modules/media-captions/dist/prod/index.js", import.meta.url);
		const src = await Bun.file(url).text();
		const snippet = src.slice(src.indexOf("const DIGIT_RE"), src.indexOf("function updateTimedVTTCueNodes"));
		render = new Function(`${snippet}; return renderVTTCueString;`)() as (cue: VTTCue) => string;
	});

	const html = (text: string) => render(new VTTCue(0, 1, text));

	test("the unsanitized payload really is an injection (guards the premise)", () => {
		expect(html("&lt;img src=x onerror=alert(1)&gt;")).toContain("<img");
	});

	// The invariant that matters: the rendered string carries no raw `<` (so innerHTML cannot build
	// an element) and no raw `"` (so nothing can break out of an attribute). The payload's own words
	// may still appear, because displaying them verbatim is the point.
	const inert = (payload: string) => {
		const out = html(sanitizeCue(payload));
		expect(out).not.toContain("<");
		expect(out).not.toContain('"');
		return out;
	};

	test("an entity-encoded element cannot create one", () => {
		inert("&lt;img src=x onerror=alert(1)&gt;");
	});

	test("a raw tag cannot create one", () => {
		inert("<img src=x onerror=alert(1)>");
	});

	test("a script tag cannot create one", () => {
		inert("&lt;script&gt;alert(1)&lt;/script&gt;");
	});

	// Annotation values are interpolated into attributes with no quote escaping.
	test("a quote-bearing voice annotation cannot inject an attribute", () => {
		inert('<v Sneaky" style="pointer-events:auto" onmouseover="alert(1)>hi</v>');
	});

	test("double-encoded entities cannot smuggle a tag through", () => {
		inert("&amp;lt;img src=x onerror=alert(1)&amp;gt;");
	});

	test("ordinary text still renders as written", () => {
		expect(html(sanitizeCue("Tom & Jerry"))).toBe("Tom &amp; Jerry");
		expect(html(sanitizeCue("a < b > c"))).toBe("a &lt; b &gt; c");
		expect(html(sanitizeCue('she said "hi"'))).toBe("she said &quot;hi&quot;");
		expect(html(sanitizeCue("[BUNNY SNORES]"))).toBe("[BUNNY SNORES]");
	});
});
