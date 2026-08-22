import { expect, test } from "bun:test";
import type * as Msf from "@moq/msf";
import { toHang } from "./msf";

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
