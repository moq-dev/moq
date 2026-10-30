import { beforeAll, describe, expect, test } from "bun:test";
import { parseText, VTTCue } from "media-captions";
import {
	type CueStore,
	clearCue,
	cueRegions,
	insertCues,
	MAX_RETAINED,
	pruneCues,
	rollUp,
	sanitizeCue,
} from "./renderer";

function store(cues: VTTCue[] = []): CueStore {
	return { cues, clears: [] };
}

// Build a cue store by inserting one payload per cue in the given delivery order, returning the
// resulting start times.
function starts(order: number[]): number[] {
	const s = store();
	for (const start of order) insertCues(s, [new VTTCue(start, start + 1, `cue ${start}`)]);
	return s.cues.map((cue) => cue.startTime);
}

// The text the renderer would show at `time`, using its own activity test.
function showing(s: CueStore, time: number): string[] {
	return s.cues.filter((cue) => time >= cue.startTime && time <= cue.endTime).map((cue) => cue.text);
}

// A VTT timestamp for a time under an hour.
function vttTime(seconds: number): string {
	// biome-ignore lint/style/noRestrictedGlobals: formats a duration as data, not a clock read.
	return new Date(seconds * 1000).toISOString().slice(11, 23);
}

describe("insertCues", () => {
	test("keeps cues sorted when they arrive in order", () => {
		expect(starts([0, 1, 2, 3])).toEqual([0, 1, 2, 3]);
	});

	// Groups are handed out in delivery order, not sequence order, so a cue can land before one
	// already stored. Appending blindly would render captions out of order.
	test("keeps cues sorted when they arrive out of order", () => {
		expect(starts([2, 0, 3, 1])).toEqual([0, 1, 2, 3]);
	});

	test("keeps equal start times in arrival order", () => {
		const s = store();
		insertCues(s, [new VTTCue(1, 2, "first")]);
		insertCues(s, [new VTTCue(1, 2, "second"), new VTTCue(1, 2, "third")]);
		expect(s.cues.map((c) => c.text)).toEqual(["first", "second", "third"]);
	});

	// Inserting each cue with a splice would make this payload quadratic: seconds, not milliseconds.
	test("merges a descending 100k-cue payload into a bounded store", () => {
		const payload: VTTCue[] = [];
		for (let i = 100_000; i > 0; i--) payload.push(new VTTCue(i, i + 1, `cue ${i}`));

		const s = store([new VTTCue(99_999, 99_999.25, "stored")]);
		insertCues(s, payload);

		expect(s.cues).toHaveLength(MAX_RETAINED);
		// The latest starts survive, one slot going to the stored cue.
		expect(s.cues[0].startTime).toBe(100_000 - MAX_RETAINED + 2);
		expect(s.cues.every((cue, i) => i === 0 || s.cues[i - 1].startTime <= cue.startTime)).toBe(true);
		expect(showing(s, 99_999.5)).toEqual(["cue 99999"]);
		expect(showing(s, 99_999.1)).toEqual(["stored", "cue 99999"]);
	});

	// Unclamped, a cue declaring a far-future end outlives every prune sweep, so a long stream of them
	// grows the store for as long as the viewer watches.
	test("a declared end cannot keep a cue past the prune horizon", () => {
		const s = store();
		for (let t = 0; t <= 600; t++) {
			insertCues(s, [new VTTCue(t, 1e9, `cue ${t}`)]);
			pruneCues(s, t - 30);
		}

		// Ends clamp to 30s past the start, then linger 30s behind the playhead before the sweep.
		expect(s.cues).toHaveLength(61);
		expect(s.cues.every((cue) => cue.endTime === cue.startTime + 30)).toBe(true);
		expect(showing(s, 600).at(-1)).toBe("cue 600");
	});

	// Timestamps that run ahead of the playhead never fall behind the prune horizon, so the ceiling is
	// what holds them; the earliest go first, and nothing is refused.
	test("a stream running ahead of the playhead stays within the ceiling", () => {
		const s = store();
		for (let i = 0; i < 2 * MAX_RETAINED; i++) {
			insertCues(s, [new VTTCue(1e6 + i, 1e6 + i + 1, `cue ${i}`)]);
			clearCue(s, 1e6 + i + 0.5);
			pruneCues(s, 0);
		}

		expect(s.cues).toHaveLength(MAX_RETAINED);
		expect(s.clears).toHaveLength(MAX_RETAINED);
		expect(s.cues[0].text).toBe(`cue ${MAX_RETAINED}`);
		expect(s.clears[0]).toBe(1e6 + MAX_RETAINED + 0.5);
	});
});

describe("cueRegions", () => {
	// Regions kept apart from their cues would outlive them, so a payload declaring a fresh region
	// per frame would grow the store, and the overlay, for the life of the stream.
	test("a fresh region per frame lives only as long as its cue", async () => {
		const s = store();
		for (let t = 0; t < 200; t++) {
			const payload = `WEBVTT\n\nREGION\nid:r${t}\n\n${vttTime(t)} --> ${vttTime(t + 1)} region:r${t}\ncue ${t}\n`;
			const parsed = await parseText(payload, { type: "vtt" });
			insertCues(s, parsed.cues);
			pruneCues(s, t - 30);
		}

		const regions = cueRegions(s.cues).map((region) => region.id);
		expect(regions).toHaveLength(32);
		expect(regions).toEqual(s.cues.map((cue) => cue.region?.id ?? ""));
		expect(showing(s, 199.5)).toEqual(["cue 199"]);
		expect(regions.at(-1)).toBe("r199");
	});
});

describe("rollUp", () => {
	// A utf8 cue has no real end: it runs until the next one starts.
	test("ends the previous cue where this one begins", () => {
		const s = store();
		const first = new VTTCue(0, 30, "first");
		insertCues(s, [first]);
		rollUp(s, first);

		const second = new VTTCue(2, 32, "second");
		insertCues(s, [second]);
		rollUp(s, second);

		expect(first.endTime).toBe(2);
		expect(second.endTime).toBe(32);
	});

	// The successor may already have arrived, in which case the late cue is the one to clamp.
	test("clamps a late cue against the successor already stored", () => {
		const s = store();
		const later = new VTTCue(4, 34, "later");
		insertCues(s, [later]);
		rollUp(s, later);

		const earlier = new VTTCue(1, 31, "earlier");
		insertCues(s, [earlier]);
		rollUp(s, earlier);

		expect(earlier.endTime).toBe(4);
		expect(later.startTime).toBe(4);
		expect(later.endTime).toBe(34);
	});

	test("leaves a lone cue lingering", () => {
		const s = store();
		const only = new VTTCue(0, 30, "only");
		insertCues(s, [only]);
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
		insertCues(s, [late]);
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
		const start = src.indexOf("const DIGIT_RE");
		const end = src.indexOf("function updateTimedVTTCueNodes");
		if (start < 0 || end < 0 || start >= end) {
			throw new Error("media-captions internals moved; update the sanitizer test extraction markers");
		}
		const snippet = src.slice(start, end);
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
