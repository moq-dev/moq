import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import type * as Msf from "@moq/msf";
import { toHang } from "./msf";

test("copies delay onto the hang rendition", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
				delay: 200,
				jitter: 40,
			},
			{
				name: "audio",
				packaging: "loc",
				role: "audio",
				codec: "opus",
				delay: 80,
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.delay).toBe(u53(200));
	expect(toHang(catalog).video?.renditions.video?.jitter).toBe(u53(40));
	expect(toHang(catalog).audio?.renditions.audio?.delay).toBe(u53(80));
});

test("preserves stalled video renditions", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
				stalled: true,
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.stalled).toBe(true);
});

test("keeps loc packaging as the loc container", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.container).toEqual({ kind: "loc" });
});

test("keeps legacy packaging as the legacy container", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "legacy",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.container).toEqual({ kind: "legacy" });
});

test("drops a cmaf rendition without a usable init segment", () => {
	const track: Msf.Track = {
		name: "video",
		packaging: "cmaf",
		role: "video",
		codec: "vp09.00.10.08",
	};

	expect(toHang({ tracks: [track] }).video?.renditions.video).toBeUndefined();
	expect(toHang({ tracks: [{ ...track, initData: "not base64!" }] }).video?.renditions.video).toBeUndefined();
});

test("drops a rendition whose packaging is unknown", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "future",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video).toBeUndefined();
});
