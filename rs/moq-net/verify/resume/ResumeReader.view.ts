// Statechart labels for ResumeReader.tla: which route the front serves,
// what the reader holds, and each step as what happens to a route or the reader.

type Copy = { t: number; rep: boolean };
type Track = { made: boolean; failed: boolean; fin: boolean; arr: Record<string, number> };
type Vars = {
	trk: Record<string, Track>;
	front: { cur: number; end: string };
	rd: { dlv: number[]; ended: string; copies: Record<string, Copy> };
};

const groups = (v: Vars) => [...v.rd.dlv].sort((a, b) => a - b);

const routes = (v: Vars) => {
	if (v.front.end === "ok") return "track declared over";
	if (v.front.end === "err") return "no route left";
	const what = (t: Track) => (t.failed ? "failed" : t.fin ? "complete" : "up");
	const serving = v.trk[String(v.front.cur)];
	const others = Object.entries(v.trk)
		.filter(([k, t]) => t.made && k !== String(v.front.cur))
		.map(([k, t]) => `route ${k} ${what(t)}`);
	const head = `route ${v.front.cur} serves${serving.fin ? ", complete" : ""}`;
	return [head, ...others].join("; ");
};

const reader = (v: Vars) => {
	const has = groups(v);
	const held = has.length ? `has ${has.join(", ")}` : "has nothing";
	if (v.rd.ended === "ok") return `ended cleanly, ${held}`;
	if (v.rd.ended === "err") return `failed, ${held}`;
	const copies = Object.values(v.rd.copies);
	const old = copies.filter((c) => c.rep).length;
	return old ? `${held}, still reading ${old} older cop${old === 1 ? "y" : "ies"}` : held;
};

export const title = "One reader of a track served across route changes";

export const state = (v: Vars) => ({ Front: routes(v), Reader: reader(v) });

const len = (r: Record<string, unknown>) => Object.keys(r).length;

export const step = (action: string, from: Vars, to: Vars) => {
	const changed = Object.keys(to.trk).find((k) => JSON.stringify(to.trk[k]) !== JSON.stringify(from.trk[k]));
	switch (action) {
		case "Up":
			return `route ${changed} comes up`;
		case "Down":
			return `route ${changed} fails`;
		case "Land": {
			const t = to.trk[changed ?? ""];
			return `group ${t.arr[String(len(t.arr))]} lands on route ${changed}`;
		}
		case "Finish":
			return `route ${changed} declares the end`;
		case "Serve":
			return `the front switches to route ${to.front.cur}`;
		case "FrontFinish":
			return "a route the front served completed";
		case "FrontFail":
			return "the front gives up";
		case "Read": {
			const got = to.rd.dlv.find((g) => !from.rd.dlv.includes(g));
			if (got !== undefined) return `the reader gets group ${got}`;
			if (to.rd.ended !== from.rd.ended) return "the reader reads the end";
			const before = len(from.rd.copies);
			const after = len(to.rd.copies);
			if (after > before) return "the reader asks the serving route for an older group it lacks";
			if (after < before) return "the reader lets an older copy go";
			return "the reader polls";
		}
		case "Drop":
			return "the reader finishes a group";
		default:
			return null;
	}
};
