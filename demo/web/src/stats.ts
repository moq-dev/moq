/**
 * MoQ relay stats dashboard.
 *
 * Every relay node that enables `[stats]` publishes a broadcast at
 * `.stats/node/<node>` carrying JSON tracks that snapshot current activity. We
 * auto-discover all of those nodes (announcements under `.stats/node`), so this
 * works for a single relay and for a cluster alike, then aggregate each node and
 * let you drill into one.
 *
 * The relay splits its stats by billing tier, an arbitrary label chosen by
 * business logic. The default tier is unprefixed; a named tier prefixes its
 * tracks with its label. This dashboard reads the two well-known tiers: the
 * default tier (external clients: JWT/public) and `internal` (mTLS /
 * cluster-connect peers), so cluster fan-out (e.g. the hub relaying between
 * nodes) shows up only in the `internal/*` tracks, never in the default numbers.
 *
 * Per-node tracks we read:
 *   publisher.json            default-tier egress  (relay -> downstream viewers)
 *   subscriber.json           default-tier ingress (upstream publishers -> relay)
 *   sessions.json             default-tier sessions by auth root
 *   internal/publisher.json   internal egress  (relay -> downstream cluster peers)
 *   internal/subscriber.json  internal ingress (upstream cluster peers -> relay)
 *   internal/sessions.json    internal sessions by auth root
 *
 * Each frame is `{ "<broadcast path>": Snapshot }`. Counters are cumulative;
 * "active" = open - closed. The relay only includes currently-live entries, so
 * the latest frame is a snapshot of now. We sample the aggregate on an interval
 * to derive per-second throughput rates for the charts.
 */

import "./highlight";
import { Net, Signals } from "@moq/hang";

const RELAY_URL = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";

// Broadcasts under this prefix are per-node stats broadcasts.
const STATS_PREFIX = ".stats/node";

// Rolling history window for the charts.
const SAMPLE_MS = 1000;
const MAX_SAMPLES = 90;

const $ = <T extends HTMLElement>(id: string): T => {
	const el = document.getElementById(id);
	if (!el) throw new Error(`missing #${id}`);
	return el as T;
};

// ---- Frame shapes (see module comment) ------------------------------------

interface Snapshot {
	announced?: number;
	announced_closed?: number;
	broadcasts?: number;
	broadcasts_closed?: number;
	subscriptions?: number;
	subscriptions_closed?: number;
	bytes?: number;
	frames?: number;
	groups?: number;
}
type BroadcastFrame = Record<string, Snapshot>;

interface SessionCounters {
	sessions?: number;
	sessions_closed?: number;
}
type SessionFrame = Record<string, SessionCounters>;

interface NodeStats {
	egress: BroadcastFrame; // publisher.json
	ingress: BroadcastFrame; // subscriber.json
	sessions: SessionFrame; // sessions.json
	internalEgress: BroadcastFrame; // internal/publisher.json
	internalIngress: BroadcastFrame; // internal/subscriber.json
	internalSessions: SessionFrame; // internal/sessions.json
}

const active = (open?: number, closed?: number) => (open ?? 0) - (closed ?? 0);

// Broadcasts whose path starts with "." are internal (e.g. the `.stats` feed
// this dashboard itself reads). We exclude them from the user-facing counters.
const isInternal = (path: string) => path.startsWith(".");

// ---- State ----------------------------------------------------------------

// Discovered nodes -> their latest stats frames.
const nodeStats = new Signals.Signal<Record<string, NodeStats>>({});
const selectedNode = new Signals.Signal<string | undefined>(undefined);

// The relay URL, editable at runtime (see the input binding below).
const relayUrl = new Signals.Signal<URL | undefined>(new URL(RELAY_URL));
const connection = new Net.Connection.Reload({ url: relayUrl, enabled: true });

// ---- Discover nodes + subscribe to each -----------------------------------

const discovery = new Signals.Effect();
discovery.run((effect) => {
	const conn = effect.get(connection.established);
	nodeStats.set({});
	if (!conn) return;

	const prefix = Net.Path.from(STATS_PREFIX);
	const announced = conn.announced(prefix);
	effect.cleanup(() => announced.close());

	// One sub-effect per node so we can tear a node's subscriptions down when it
	// goes away (e.g. a cluster peer disconnects).
	const subs = new Map<string, Signals.Effect>();
	effect.cleanup(() => {
		for (const e of subs.values()) e.close();
	});

	effect.spawn(async () => {
		for (;;) {
			const entry = await Promise.race([effect.cancel, announced.next()]);
			if (!entry) break;
			const node = (Net.Path.stripPrefix(prefix, entry.path) ?? entry.path) as string;
			if (!node) continue;

			if (entry.active) {
				if (subs.has(node)) continue;
				const ne = new Signals.Effect();
				subs.set(node, ne);
				subscribeNode(ne, conn, entry.path, node);
			} else {
				subs.get(node)?.close();
				subs.delete(node);
				nodeStats.mutate((s) => {
					delete s[node];
				});
			}
		}
	});
});

function subscribeNode(effect: Signals.Effect, conn: Net.Connection.Established, path: Net.Path.Valid, node: string) {
	nodeStats.mutate((s) => {
		s[node] = {
			egress: {},
			ingress: {},
			sessions: {},
			internalEgress: {},
			internalIngress: {},
			internalSessions: {},
		};
	});

	const consumer = conn.consume(path);
	effect.cleanup(() => consumer.close());

	const sub = <K extends keyof NodeStats>(trackName: string, key: K) => {
		const track = consumer.subscribe(trackName, 0);
		effect.cleanup(() => track.close());
		effect.spawn(async () => {
			for (;;) {
				const data = await Promise.race([effect.cancel, track.readJson()]);
				if (data === undefined) break;
				nodeStats.mutate((s) => {
					const cur = s[node];
					if (cur) cur[key] = (data ?? {}) as NodeStats[K];
				});
			}
		});
	};

	sub("publisher.json", "egress");
	sub("subscriber.json", "ingress");
	sub("sessions.json", "sessions");
	sub("internal/publisher.json", "internalEgress");
	sub("internal/subscriber.json", "internalIngress");
	sub("internal/sessions.json", "internalSessions");
}

// ---- Aggregation ----------------------------------------------------------

// Aggregate one ingress/egress pair (either the external or the internal tier).
// `.`-prefixed system broadcasts (the `.stats` feed itself) are excluded either
// way; we only count real content, including its cluster fan-out.
function aggregatePair(ingress: BroadcastFrame, egress: BroadcastFrame) {
	let broadcasters = 0; // active broadcasts being published (ingress)
	let viewers = 0; // active downstream consumers (egress)
	let ingressBytes = 0;
	let egressBytes = 0;

	for (const [path, s] of Object.entries(ingress)) {
		if (isInternal(path)) continue;
		if (active(s.announced, s.announced_closed) > 0) broadcasters++;
		ingressBytes += s.bytes ?? 0;
	}
	for (const [path, s] of Object.entries(egress)) {
		if (isInternal(path)) continue;
		egressBytes += s.bytes ?? 0;
		viewers += active(s.broadcasts, s.broadcasts_closed);
	}
	return { broadcasters, viewers, ingressBytes, egressBytes };
}

function aggregate(stats: NodeStats) {
	return {
		external: aggregatePair(stats.ingress, stats.egress),
		internal: aggregatePair(stats.internalIngress, stats.internalEgress),
	};
}

const countSessions = (f: SessionFrame) =>
	Object.values(f).reduce((n, s) => n + active(s.sessions, s.sessions_closed), 0);

// ---- Time-series history ---------------------------------------------------

// One sample captures cumulative byte counters (for rate charts) plus the
// instantaneous gauges. Keyed by node name, with "" reserved for the
// cluster-wide aggregate.
interface Sample {
	t: number;
	egress: number; // cumulative external egress bytes
	ingress: number; // cumulative external ingress bytes
	clusterOut: number; // cumulative internal egress bytes
	clusterIn: number; // cumulative internal ingress bytes
	broadcasters: number;
	viewers: number;
	sessions: number;
}

const history = new Map<string, Sample[]>();
// Bumped every sample so the chart effects re-render without making the whole
// history map reactive.
const clock = new Signals.Signal(0);

function sampleNode(t: number, stats: NodeStats): Sample {
	const a = aggregate(stats);
	return {
		t,
		egress: a.external.egressBytes,
		ingress: a.external.ingressBytes,
		clusterOut: a.internal.egressBytes,
		clusterIn: a.internal.ingressBytes,
		broadcasters: a.external.broadcasters,
		viewers: a.external.viewers,
		sessions: countSessions(stats.sessions),
	};
}

function pushSample(key: string, s: Sample) {
	const arr = history.get(key) ?? [];
	arr.push(s);
	if (arr.length > MAX_SAMPLES) arr.shift();
	history.set(key, arr);
}

// The set of nodes in the most recent cluster-aggregate sample. The aggregate
// sums cumulative per-node counters, so when membership changes the summed
// baseline jumps; we reset the aggregate series rather than splice the jump in.
let clusterMembership = "";

const sampler = new Signals.Effect();
sampler.run((effect) => {
	// Only sample while connected; the interval restarts on reconnect. Drop the
	// rolling history when disconnected so a reconnect doesn't splice new
	// samples onto stale ones across the downtime gap.
	if (!effect.get(connection.established)) {
		history.clear();
		clusterMembership = "";
		clock.update((n) => n + 1);
		return;
	}
	effect.interval(() => {
		const all = nodeStats.peek();
		const nodes = Object.keys(all).sort();
		const t = Date.now();

		const agg: Sample = {
			t,
			egress: 0,
			ingress: 0,
			clusterOut: 0,
			clusterIn: 0,
			broadcasters: 0,
			viewers: 0,
			sessions: 0,
		};
		for (const node of nodes) {
			const s = sampleNode(t, all[node] as NodeStats);
			pushSample(node, s);
			agg.egress += s.egress;
			agg.ingress += s.ingress;
			agg.clusterOut += s.clusterOut;
			agg.clusterIn += s.clusterIn;
			agg.broadcasters += s.broadcasters;
			agg.viewers += s.viewers;
			agg.sessions += s.sessions;
		}
		// A changed node set makes this aggregate's baseline incompatible with the
		// previous one, so start the cluster series fresh instead of splicing.
		const membership = nodes.join("\0");
		if (membership !== clusterMembership) {
			history.delete("");
			clusterMembership = membership;
		}
		pushSample("", agg);

		// Drop history for nodes that have gone away.
		for (const key of history.keys()) {
			if (key !== "" && !nodes.includes(key)) history.delete(key);
		}

		clock.update((n) => n + 1);
	}, SAMPLE_MS);
});

// Convert a cumulative-counter series into a per-second rate series.
function rateSeries(samples: Sample[], field: keyof Sample): number[] {
	const out: number[] = [];
	for (let i = 1; i < samples.length; i++) {
		const dt = (samples[i].t - samples[i - 1].t) / 1000;
		const delta = (samples[i][field] as number) - (samples[i - 1][field] as number);
		out.push(dt > 0 ? Math.max(0, delta / dt) : 0);
	}
	return out;
}

const lastRate = (samples: Sample[], field: keyof Sample): number => {
	const r = rateSeries(samples, field);
	return r.length ? (r[r.length - 1] as number) : 0;
};

// ---- Render ---------------------------------------------------------------

const ui = new Signals.Effect();

// Relay URL is editable: committing a new value reconnects the dashboard.
const relayEl = $<HTMLInputElement>("relay-url");
relayEl.value = RELAY_URL;
ui.run((effect) => {
	effect.event(relayEl, "change", () => {
		try {
			relayUrl.set(new URL(relayEl.value.trim()));
		} catch {
			// Revert invalid input to the last good URL.
			relayEl.value = relayUrl.peek()?.toString() ?? RELAY_URL;
		}
	});
});

ui.run((effect) => {
	const status = effect.get(connection.status);
	const el = $("status");
	const dot = status === "connected" ? "bg-emerald-400" : status === "connecting" ? "bg-amber-400" : "bg-red-400";
	const tone =
		status === "connected"
			? "text-emerald-300 border-emerald-800"
			: status === "connecting"
				? "text-amber-300 border-amber-800"
				: "text-red-300 border-red-800";
	el.className = `inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs font-medium bg-neutral-900 border ${tone}`;
	el.replaceChildren(spanDot(dot), document.createTextNode(status));
});

// Keep a valid selection: default to the first node, switch away from one that
// disappears.
ui.run((effect) => {
	const nodes = Object.keys(effect.get(nodeStats)).sort();
	const cur = selectedNode.peek();
	if (cur && nodes.includes(cur)) return;
	selectedNode.set(nodes[0]);
});

// Cluster summary cards.
ui.run((effect) => {
	effect.get(clock);
	const all = nodeStats.peek();
	const nodes = Object.keys(all);
	const cluster = history.get("") ?? [];
	const latest = cluster[cluster.length - 1];

	$("kpi-nodes").textContent = String(nodes.length);
	$("kpi-broadcasters").textContent = String(latest?.broadcasters ?? 0);
	$("kpi-viewers").textContent = String(latest?.viewers ?? 0);
	$("kpi-sessions").textContent = String(latest?.sessions ?? 0);
	$("kpi-egress").textContent = formatRate(lastRate(cluster, "egress"));
	$("kpi-ingress").textContent = formatRate(lastRate(cluster, "ingress"));
});

// Cluster throughput chart.
ui.run((effect) => {
	effect.get(clock);
	const cluster = history.get("") ?? [];
	const egress = rateSeries(cluster, "egress");
	const ingress = rateSeries(cluster, "ingress");
	const peer = rateSeries(cluster, "clusterOut");

	renderChart($("chart-throughput"), [
		{ values: egress, color: "#34d399" }, // emerald-400
		{ values: ingress, color: "#38bdf8" }, // sky-400
		{ values: peer, color: "#a78bfa" }, // violet-400
	]);
	$("legend-egress").textContent = formatRate(egress[egress.length - 1] ?? 0);
	$("legend-ingress").textContent = formatRate(ingress[ingress.length - 1] ?? 0);
	$("legend-cluster").textContent = formatRate(peer[peer.length - 1] ?? 0);
});

// Node cards: one per node with a live egress sparkline. Click to drill in.
// Driven by `clock` (sampled cadence), not raw `nodeStats` frames, so cards
// don't rebuild (and drop focus) on every incoming frame.
ui.run((effect) => {
	effect.get(clock);
	const all = nodeStats.peek();
	const sel = effect.get(selectedNode);
	const nodes = Object.keys(all).sort();
	const el = $("nodes");

	if (nodes.length === 0) {
		el.textContent = "searching for nodes…";
		el.className = "text-neutral-500 text-sm";
		return;
	}
	el.className = "grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3";
	el.replaceChildren(...nodes.map((node) => nodeCard(effect, node, history.get(node) ?? [], node === sel)));
});

// Drill-down for the selected node.
ui.run((effect) => {
	effect.get(clock);
	const all = effect.get(nodeStats);
	const node = effect.get(selectedNode);
	const detail = $("node-detail");
	const stats = node ? all[node] : undefined;

	if (!node || !stats) {
		detail.hidden = true;
		return;
	}
	detail.hidden = false;
	$("node-title").textContent = node;

	const samples = history.get(node) ?? [];
	renderChart($("chart-node-external"), [
		{ values: rateSeries(samples, "egress"), color: "#34d399" },
		{ values: rateSeries(samples, "ingress"), color: "#38bdf8" },
	]);
	renderChart($("chart-node-cluster"), [
		{ values: rateSeries(samples, "clusterOut"), color: "#a78bfa" },
		{ values: rateSeries(samples, "clusterIn"), color: "#e879f9" }, // fuchsia-400
	]);

	// Broadcasters: what this node ingests (from upstream publishers / cluster peers).
	const ingressRows = (frame: BroadcastFrame) =>
		Object.keys(frame)
			.filter((p) => !isInternal(p))
			.sort()
			.map((path) => {
				const i = frame[path] ?? {};
				return { key: path, cells: [path, formatBytes(i.bytes ?? 0), String(i.frames ?? 0)] };
			});

	// Viewers: what this node serves downstream (to subscribers / cluster peers).
	const egressRows = (frame: BroadcastFrame) =>
		Object.keys(frame)
			.filter((p) => !isInternal(p))
			.sort()
			.map((path) => {
				const e = frame[path] ?? {};
				return {
					key: path,
					cells: [
						path,
						String(active(e.broadcasts, e.broadcasts_closed)), // viewers / peers
						formatBytes(e.bytes ?? 0), // egress
						String(e.frames ?? 0),
					],
				};
			});

	renderTable($("node-publishers"), ["broadcast", "ingress", "frames"], ingressRows(stats.ingress));
	renderTable($("node-subscribers"), ["broadcast", "viewers", "egress", "frames"], egressRows(stats.egress));
	renderTable($("node-internal-publishers"), ["broadcast", "ingress", "frames"], ingressRows(stats.internalIngress));
	renderTable(
		$("node-internal-subscribers"),
		["broadcast", "peers", "egress", "frames"],
		egressRows(stats.internalEgress),
	);

	const sessions = countSessions(stats.sessions);
	const internalSessions = countSessions(stats.internalSessions);
	$("node-sessions").textContent = `${sessions} session${sessions === 1 ? "" : "s"}`;
	$("node-internal-sessions").textContent = `${internalSessions} cluster`;
});

// Raw frames for everyone who wants the numbers behind the charts.
ui.run((effect) => {
	$("raw").textContent = JSON.stringify(effect.get(nodeStats), null, 2);
});

// ---- DOM helpers -----------------------------------------------------------

function spanDot(colorClass: string): HTMLSpanElement {
	const s = document.createElement("span");
	s.className = `inline-block w-2 h-2 rounded-full ${colorClass}`;
	return s;
}

// A clickable card summarizing one node, with a live egress sparkline.
function nodeCard(effect: Signals.Effect, node: string, samples: Sample[], selected: boolean): HTMLElement {
	const latest = samples[samples.length - 1];
	const card = document.createElement("div");
	card.className = [
		"rounded-lg border bg-neutral-900/50 p-3 cursor-pointer transition-colors",
		selected ? "border-emerald-600 ring-1 ring-emerald-600/40" : "border-neutral-800 hover:border-neutral-600",
	].join(" ");
	card.tabIndex = 0;
	card.setAttribute("role", "button");

	const head = document.createElement("div");
	head.className = "flex items-center justify-between gap-2 mb-2";
	const name = document.createElement("span");
	name.className = "font-mono text-sm text-neutral-200 truncate";
	name.textContent = node;
	const rate = document.createElement("span");
	rate.className = "text-sm font-semibold tabular-nums text-emerald-400 whitespace-nowrap";
	rate.textContent = formatRate(lastRate(samples, "egress"));
	head.append(name, rate);

	const spark = makeChart([{ values: rateSeries(samples, "egress"), color: "#34d399" }], 200, 36);
	spark.classList.add("w-full", "h-9", "mb-2");

	const stats = document.createElement("div");
	stats.className = "flex items-center gap-4 text-xs text-neutral-400";
	stats.append(
		stat("broadcasters", latest?.broadcasters ?? 0, "text-sky-300"),
		stat("viewers", latest?.viewers ?? 0, "text-emerald-300"),
		stat("sessions", latest?.sessions ?? 0, "text-neutral-200"),
	);

	card.append(head, spark, stats);

	const activate = () => selectedNode.set(node);
	effect.event(card, "click", activate);
	effect.event(card, "keydown", (e) => {
		if (e.key === "Enter" || e.key === " ") {
			e.preventDefault();
			activate();
		}
	});
	return card;
}

function stat(label: string, value: number, valueClass: string): HTMLElement {
	const wrap = document.createElement("span");
	wrap.className = "flex items-center gap-1";
	const v = document.createElement("span");
	v.className = `font-semibold tabular-nums ${valueClass}`;
	v.textContent = String(value);
	const l = document.createElement("span");
	l.className = "text-neutral-500";
	l.textContent = label;
	wrap.append(v, l);
	return wrap;
}

interface Row {
	key: string;
	cells: string[];
}

function renderTable(container: HTMLElement, headers: string[], rows: Row[]) {
	if (rows.length === 0) {
		container.textContent = "none";
		container.className = "text-neutral-600 text-xs italic";
		return;
	}
	container.className = "overflow-x-auto";
	const table = document.createElement("table");
	table.className = "w-full text-xs border-collapse";

	const thead = document.createElement("thead");
	const htr = document.createElement("tr");
	for (const h of headers) {
		const th = document.createElement("th");
		th.className = "text-left font-medium text-neutral-500 px-2 py-1 border-b border-neutral-800";
		th.textContent = h;
		htr.appendChild(th);
	}
	thead.appendChild(htr);
	table.appendChild(thead);

	const tbody = document.createElement("tbody");
	for (const row of rows) {
		const tr = document.createElement("tr");
		tr.className = "border-b border-neutral-900";
		for (const cell of row.cells) {
			const td = document.createElement("td");
			td.className = "px-2 py-1 font-mono text-neutral-300 whitespace-nowrap";
			td.textContent = cell;
			tr.appendChild(td);
		}
		tbody.appendChild(tr);
	}
	table.appendChild(tbody);
	container.replaceChildren(table);
}

// ---- SVG charts ------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";

// Monotonic counter for gradient element ids. SVG ids are document-global and
// every chart re-renders into the same page, so the id must be unique per
// gradient instance, not derived from the series (which collides across charts
// and would make `url(#id)` resolve to the wrong gradient).
let gradSeq = 0;

interface Series {
	values: number[];
	color: string;
}

// Render a multi-series area/line chart into a host element, filling its width.
function renderChart(host: HTMLElement, series: Series[]) {
	const rect = host.getBoundingClientRect();
	// Fall back to a sane height if the host hasn't been laid out yet (0px).
	const h = Math.round(rect.height) || 120;
	const svg = makeChart(series, 600, h);
	svg.classList.add("w-full", "h-full");
	host.replaceChildren(svg);
}

// Build an SVG chart in a `vw`×`vh` viewBox. All series share one vertical
// scale so they're directly comparable. `preserveAspectRatio="none"` lets it
// stretch to any container width; only straight lines are drawn so the
// non-uniform scaling stays invisible.
function makeChart(series: Series[], vw: number, vh: number): SVGSVGElement {
	const svg = document.createElementNS(SVG_NS, "svg");
	svg.setAttribute("viewBox", `0 0 ${vw} ${vh}`);
	svg.setAttribute("preserveAspectRatio", "none");

	const lens = series.map((s) => s.values.length);
	const maxLen = Math.max(0, ...lens);
	const maxVal = Math.max(1, ...series.flatMap((s) => s.values));

	// Baseline along the bottom even before data arrives.
	const baseline = document.createElementNS(SVG_NS, "line");
	baseline.setAttribute("x1", "0");
	baseline.setAttribute("y1", String(vh - 1));
	baseline.setAttribute("x2", String(vw));
	baseline.setAttribute("y2", String(vh - 1));
	baseline.setAttribute("stroke", "#404040"); // neutral-700
	baseline.setAttribute("stroke-width", "1");
	svg.appendChild(baseline);

	if (maxLen < 2) return svg;

	const pad = 2;
	const x = (i: number) => (i / (maxLen - 1)) * vw;
	const y = (v: number) => vh - pad - (v / maxVal) * (vh - 2 * pad);

	for (const s of series) {
		if (s.values.length < 2) continue;
		const pts = s.values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`);

		const gradId = `moq-grad-${gradSeq++}`;
		const grad = document.createElementNS(SVG_NS, "linearGradient");
		grad.setAttribute("id", gradId);
		grad.setAttribute("x1", "0");
		grad.setAttribute("y1", "0");
		grad.setAttribute("x2", "0");
		grad.setAttribute("y2", "1");
		for (const [offset, opacity] of [
			["0%", "0.35"],
			["100%", "0"],
		]) {
			const stop = document.createElementNS(SVG_NS, "stop");
			stop.setAttribute("offset", offset);
			stop.setAttribute("stop-color", s.color);
			stop.setAttribute("stop-opacity", opacity);
			grad.appendChild(stop);
		}
		svg.appendChild(grad);

		const area = document.createElementNS(SVG_NS, "path");
		area.setAttribute("d", `M0,${vh} L${pts.join(" L")} L${vw},${vh} Z`);
		area.setAttribute("fill", `url(#${gradId})`);
		svg.appendChild(area);

		const line = document.createElementNS(SVG_NS, "polyline");
		line.setAttribute("points", pts.join(" "));
		line.setAttribute("fill", "none");
		line.setAttribute("stroke", s.color);
		line.setAttribute("stroke-width", "1.5");
		line.setAttribute("vector-effect", "non-scaling-stroke");
		line.setAttribute("stroke-linejoin", "round");
		svg.appendChild(line);
	}

	return svg;
}

// ---- Formatting ------------------------------------------------------------

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Throughput as bits/second (Mbps is how operators think about a relay).
function formatRate(bytesPerSec: number): string {
	const bits = bytesPerSec * 8;
	if (bits < 1000) return `${Math.round(bits)} bps`;
	if (bits < 1_000_000) return `${(bits / 1000).toFixed(0)} kbps`;
	if (bits < 1_000_000_000) return `${(bits / 1_000_000).toFixed(1)} Mbps`;
	return `${(bits / 1_000_000_000).toFixed(2)} Gbps`;
}

// Vite re-evaluates this module on hot reload, dropping the references to the
// module-scoped effects/connection above. Close them on dispose so they don't
// get garbage collected unclosed (which the signals library warns about).
if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		for (const effect of [discovery, sampler, ui]) effect.close();
		connection.close();
	});
}
