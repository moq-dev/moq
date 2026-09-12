/**
 * Conferencing demo on @moq/room: a room is a path prefix, participants are
 * discovered from the announce stream, each publishes camera and optional screen.
 */

import "@moq/publish/support/element";
import "@moq/watch/support/element";
import { Local, type Member, Net, Publish, Room, Signals } from "@moq/room";

const RELAY_URL = import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443";

const $ = <T extends HTMLElement>(id: string): T => {
	const el = document.getElementById(id);
	if (!el) throw new Error(`missing #${id}`);
	return el as T;
};

function segment(raw: string, fallback: string): string {
	const cleaned = raw
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9-]+/g, "-")
		.replace(/^-+|-+$/g, "")
		.slice(0, 32);
	return cleaned || fallback;
}

function randomName(): string {
	return `p-${Math.random().toString(36).slice(2, 8)}`;
}

const params = new URLSearchParams(location.search);
const roomInput = $<HTMLInputElement>("room");
const nameInput = $<HTMLInputElement>("name");
const relayEl = $<HTMLInputElement>("relay-url");
roomInput.value = params.get("room") ?? "demo";
nameInput.value = params.get("name") ?? randomName();
relayEl.value = RELAY_URL;

const ui = new Signals.Effect();
const joined = new Signals.Signal(false);
const tiles = new Map<string, { element: HTMLElement; label: HTMLElement }>();
const tilesEl = $("tiles");
const emptyEl = $("tiles-empty");

let session: Signals.Effect | undefined;
let connection: Net.Connection.Reload | undefined;
let local: Local | undefined;
let room: Room | undefined;
let localPreview: Publish.Preview.Renderer | undefined;

function setPill(label: string, state: "ok" | "wait" | "bad"): void {
	$("conn-text").textContent = label;
	const dot = $("conn-status").querySelector(".dot") as HTMLElement;
	const color = state === "ok" ? "bg-emerald-500" : state === "wait" ? "bg-amber-400" : "bg-red-500";
	dot.className = `dot w-2 h-2 rounded-full ${color}`;
}

function tile(id: string, title: string, canvas: HTMLCanvasElement, you = false): HTMLElement {
	const el = document.createElement("div");
	el.className = "rounded-lg overflow-hidden border border-neutral-800 bg-neutral-900";
	const label = document.createElement("div");
	label.className = "px-3 py-1.5 text-xs font-mono text-neutral-300 border-b border-neutral-800 truncate";
	label.textContent = you ? `${title} (you)` : title;
	canvas.className = "w-full h-auto bg-black";
	canvas.style.aspectRatio = "16 / 9";
	el.append(label, canvas);
	tiles.set(id, { element: el, label });
	tilesEl.append(el);
	emptyEl.hidden = true;
	return el;
}

function dropTile(id: string): void {
	tiles.get(id)?.element.remove();
	tiles.delete(id);
	emptyEl.hidden = tiles.size > 0;
}

function roomUrl(relay: string, roomName: string): URL {
	const base = relay.replace(/\/+$/, "");
	return new URL(`${base}/anon/meet/${roomName}`);
}

function leave(): void {
	session?.close();
	session = undefined;
	localPreview?.close();
	localPreview = undefined;
	room?.close();
	room = undefined;
	local?.close();
	local = undefined;
	connection?.close();
	connection = undefined;
	for (const id of [...tiles.keys()]) dropTile(id);
	$("controls").hidden = true;
	$("join").textContent = "Join";
	joined.set(false);
	setPill("Disconnected", "bad");
}

function join(): void {
	leave();

	const roomName = segment(roomInput.value, "demo");
	const name = segment(nameInput.value, randomName());
	roomInput.value = roomName;
	nameInput.value = name;

	const next = new URL(location.href);
	next.searchParams.set("room", roomName);
	next.searchParams.set("name", name);
	history.replaceState(undefined, "", next);

	let relay: URL;
	try {
		relay = new URL(relayEl.value.trim());
	} catch {
		relayEl.value = RELAY_URL;
		relay = new URL(RELAY_URL);
	}

	const identity = Net.Path.from(name);
	connection = new Net.Connection.Reload({
		url: roomUrl(relay.toString(), roomName),
		enabled: true,
	});
	local = new Local({
		connection: connection.established,
		identity,
		enabled: true,
		user: { id: name, name },
	});
	local.cameraEnabled.set(true);
	local.microphoneEnabled.set(true);
	room = new Room({ connection, identity });

	const localCanvas = document.createElement("canvas");
	tile("local", name, localCanvas, true);
	localPreview = new Publish.Preview.Renderer({
		canvas: localCanvas,
		frame: local.cameraCapture.out.frame,
		display: local.cameraCapture.out.display,
		flip: true,
	});

	$("controls").hidden = false;
	$("join").textContent = "Leave";
	joined.set(true);

	session = new Signals.Effect();
	session.run((effect) => {
		if (!connection) return;
		const status = effect.get(connection.status);
		const label = status.charAt(0).toUpperCase() + status.slice(1);
		setPill(label, status === "connected" ? "ok" : status === "connecting" ? "wait" : "bad");
	});

	const members = new Map<string, Member>();
	session.run((effect) => {
		if (!room) return;
		const remotes = effect.get(room.remotes);
		const live = new Set<string>();

		for (const [id, remote] of remotes) {
			for (const member of [effect.get(remote.camera), effect.get(remote.screen)]) {
				if (!member) continue;
				const key = `${id}/${member.kind}`;
				live.add(key);
				const title =
					member.kind === "screen"
						? `${effect.get(remote.user.name) ?? id} screen`
						: (effect.get(remote.user.name) ?? id);
				const previous = members.get(key);
				if (previous === member) {
					const existing = tiles.get(key);
					if (existing) existing.label.textContent = title;
					continue;
				}
				previous?.canvas.set(undefined);
				dropTile(key);
				members.set(key, member);
				const canvas = document.createElement("canvas");
				tile(key, title, canvas);
				member.canvas.set(canvas);
				member.muted.set(false);
			}
		}

		for (const [key, member] of members) {
			if (live.has(key)) continue;
			member.canvas.set(undefined);
			members.delete(key);
			dropTile(key);
		}
	});
}

$("join").addEventListener("click", () => {
	if (joined.peek()) leave();
	else join();
});

function arm(id: string, pick: () => Signals.Signal<boolean> | undefined): void {
	const button = $<HTMLButtonElement>(id);
	button.addEventListener("click", () => {
		const s = pick();
		if (!s) return;
		s.set(!s.peek());
	});
	ui.run((effect) => {
		effect.get(joined);
		const s = pick();
		const on = s ? effect.get(s) : false;
		button.classList.toggle("bg-emerald-700", on);
		button.classList.toggle("hover:bg-emerald-600", on);
		button.classList.toggle("bg-neutral-800", !on);
	});
}

arm("toggle-camera", () => local?.cameraEnabled);
arm("toggle-mic", () => local?.microphoneEnabled);
arm("toggle-screen", () => local?.screenEnabled);

if (import.meta.hot) {
	import.meta.hot.dispose(() => {
		leave();
		ui.close();
	});
}
