/**
 * MoQ relay stats dashboard.
 *
 * Every relay node that enables `[stats]` publishes a broadcast at
 * `.stats/node/<node>` carrying JSON tracks that snapshot current activity. We
 * auto-discover all of those nodes (announcements under `.stats/node`), so this
 * works for a single relay and for a cluster alike, then aggregate each node and
 * let you drill into one.
 *
 * The relay splits its stats by billing tier: external clients (JWT/public) and
 * internal peers (mTLS / cluster-connect). It publishes a parallel set of tracks
 * for each, so cluster fan-out (e.g. the hub relaying between nodes) shows up
 * only in the `internal/*` tracks, never in the external numbers.
 *
 * Per-node tracks we read:
 *   publisher.json            external egress  (relay -> downstream viewers)
 *   subscriber.json           external ingress (upstream publishers -> relay)
 *   sessions.json             external sessions by auth root
 *   internal/publisher.json   internal egress  (relay -> downstream cluster peers)
 *   internal/subscriber.json  internal ingress (upstream cluster peers -> relay)
 *   internal/sessions.json    internal sessions by auth root
 *
 * Each frame is `{ "<broadcast path>": Snapshot }`. Counters are cumulative;
 * "active" = open - closed. The relay only includes currently-live entries, so
 * the latest frame is a snapshot of now.
 */

import "./highlight";
import { Moq, Signals } from "@moq/hang";

const RELAY_URL = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";

// Broadcasts under this prefix are per-node stats broadcasts.
const STATS_PREFIX = ".stats/node";

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
const connection = new Moq.Connection.Reload({ url: relayUrl, enabled: true });

// ---- Discover nodes + subscribe to each -----------------------------------

const discovery = new Signals.Effect();
discovery.run((effect) => {
	const conn = effect.get(connection.established);
	nodeStats.set({});
	if (!conn) return;

	const prefix = Moq.Path.from(STATS_PREFIX);
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
			const node = (Moq.Path.stripPrefix(prefix, entry.path) ?? entry.path) as string;
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

function subscribeNode(effect: Signals.Effect, conn: Moq.Connection.Established, path: Moq.Path.Valid, node: string) {
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
	el.textContent = status;
	const color =
		status === "connected"
			? "text-emerald-400 border-emerald-700"
			: status === "connecting"
				? "text-amber-400 border-amber-700"
				: "text-red-400 border-red-700";
	el.className = `inline-flex items-center px-2 py-1 rounded text-xs bg-neutral-900 border ${color}`;
});

// Keep a valid selection: default to the first node, switch away from one that
// disappears.
ui.run((effect) => {
	const nodes = Object.keys(effect.get(nodeStats)).sort();
	const cur = selectedNode.peek();
	if (cur && nodes.includes(cur)) return;
	selectedNode.set(nodes[0]);
});

// Aggregate table: one row per node, click to drill in.
ui.run((effect) => {
	const all = effect.get(nodeStats);
	const sel = effect.get(selectedNode);
	const nodes = Object.keys(all).sort();
	const el = $("nodes");

	if (nodes.length === 0) {
		el.textContent = "searching for nodes…";
		return;
	}

	const headers = ["node", "broadcasters", "viewers", "ingress", "egress", "cluster in", "cluster out"];
	const rows = nodes.map((node) => {
		const a = aggregate(all[node] as NodeStats);
		return {
			key: node,
			cells: [
				node,
				String(a.external.broadcasters),
				String(a.external.viewers),
				formatBytes(a.external.ingressBytes),
				formatBytes(a.external.egressBytes),
				formatBytes(a.internal.ingressBytes),
				formatBytes(a.internal.egressBytes),
			],
		};
	});
	renderTable(effect, el, headers, rows, { selected: sel, onClick: (k) => selectedNode.set(k) });
});

// Drill-down: the selected node's broadcasts (egress-focused) + sessions.
ui.run((effect) => {
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

	// Broadcasters: what this node ingests (from upstream publishers / cluster peers).
	const ingressRows = (frame: BroadcastFrame) =>
		Object.keys(frame)
			.filter((p) => !isInternal(p))
			.sort()
			.map((path) => {
				const i = frame[path] ?? {};
				return {
					key: path,
					cells: [path, formatBytes(i.bytes ?? 0), String(i.frames ?? 0), String(i.groups ?? 0)],
				};
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
						String(active(e.subscriptions, e.subscriptions_closed)), // track subs
						formatBytes(e.bytes ?? 0), // egress
						String(e.frames ?? 0),
						String(e.groups ?? 0),
					],
				};
			});

	const inHeaders = ["broadcast", "ingress", "frames", "groups"];
	const outHeaders = ["broadcast", "viewers", "track subs", "egress", "frames", "groups"];

	renderTable(effect, $("node-publishers"), inHeaders, ingressRows(stats.ingress));
	renderTable(effect, $("node-subscribers"), outHeaders, egressRows(stats.egress));
	renderTable(
		effect,
		$("node-internal-publishers"),
		["broadcast", "ingress", "frames", "groups"],
		ingressRows(stats.internalIngress),
	);
	renderTable(
		effect,
		$("node-internal-subscribers"),
		["broadcast", "peers", "track subs", "egress", "frames", "groups"],
		egressRows(stats.internalEgress),
	);

	const countSessions = (f: SessionFrame) =>
		Object.values(f).reduce((n, s) => n + active(s.sessions, s.sessions_closed), 0);
	const sessions = countSessions(stats.sessions);
	const internalSessions = countSessions(stats.internalSessions);
	$("node-sessions").textContent = `${sessions} external session${sessions === 1 ? "" : "s"}`;
	$("node-internal-sessions").textContent = `${internalSessions} cluster session${internalSessions === 1 ? "" : "s"}`;
});

// Raw frames for everyone who wants the numbers behind the tables.
ui.run((effect) => {
	$("raw").textContent = JSON.stringify(effect.get(nodeStats), null, 2);
});

// ---- Helpers --------------------------------------------------------------

interface Row {
	key: string;
	cells: string[];
}

function renderTable(
	effect: Signals.Effect,
	container: HTMLElement,
	headers: string[],
	rows: Row[],
	opts?: { selected?: string; onClick?: (key: string) => void },
) {
	if (rows.length === 0) {
		container.textContent = "no active entries";
		return;
	}
	const table = document.createElement("table");
	table.className = "w-full text-sm border-collapse";

	const thead = document.createElement("thead");
	const htr = document.createElement("tr");
	for (const h of headers) {
		const th = document.createElement("th");
		th.className = "text-left font-medium text-neutral-400 px-2 py-1 border-b border-neutral-700";
		th.textContent = h;
		htr.appendChild(th);
	}
	thead.appendChild(htr);
	table.appendChild(thead);

	const tbody = document.createElement("tbody");
	for (const row of rows) {
		const tr = document.createElement("tr");
		tr.className = "border-b border-neutral-800";
		if (opts?.onClick) {
			tr.classList.add("cursor-pointer", "hover:bg-neutral-800");
			tr.tabIndex = 0;
			tr.setAttribute("role", "button");
			if (row.key === opts.selected) tr.classList.add("bg-neutral-800", "text-emerald-300");
			const activate = () => opts.onClick?.(row.key);
			effect.event(tr, "click", activate);
			effect.event(tr, "keydown", (e) => {
				if (e.key === "Enter" || e.key === " ") {
					e.preventDefault();
					activate();
				}
			});
		}
		for (const cell of row.cells) {
			const td = document.createElement("td");
			td.className = "px-2 py-1 font-mono text-neutral-200 whitespace-nowrap";
			td.textContent = cell;
			tr.appendChild(td);
		}
		tbody.appendChild(tr);
	}
	table.appendChild(tbody);
	container.replaceChildren(table);
}

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
	return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Vite re-evaluates this module on hot reload, dropping the references to the
// module-scoped effects/connection above. Close them on dispose so they don't
// get garbage collected unclosed (which the signals library warns about).
if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		for (const effect of [discovery, ui]) effect.close();
		connection.close();
	});
}
