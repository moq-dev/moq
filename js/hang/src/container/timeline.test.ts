import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { Segmenter } from "./timeline.ts";

const us = (ms: number): Time.Micro => (ms * 1000) as Time.Micro;

test("boundary opens a segment per group", () => {
	const segmenter = new Segmenter();
	expect(segmenter.boundary(us(0))).toBe(0);
	expect(segmenter.boundary(us(2000))).toBe(1);
	expect(segmenter.boundary(us(4000))).toBe(2);
});

test("aligned records the first group at each boundary", () => {
	const segmenter = new Segmenter();
	let last: number | undefined;
	const align = (ms: number): number | undefined => {
		const segment = segmenter.align(us(ms), last);
		if (segment !== undefined) last = segment;
		return segment;
	};

	expect(segmenter.boundary(us(0))).toBe(0);
	expect(align(0)).toBe(0);
	// Groups inside the segment extend it (no record).
	expect(align(300)).toBeUndefined();
	expect(align(600)).toBeUndefined();

	expect(segmenter.boundary(us(2000))).toBe(1);
	// The first group at/after the boundary starts the track's slice of segment 1.
	expect(align(2100)).toBe(1);
	expect(align(2400)).toBeUndefined();
});

test("undriven aligned producer paces itself at the interval", () => {
	const segmenter = new Segmenter();
	let last: number | undefined;
	const align = (ms: number): number | undefined => {
		const segment = segmenter.align(us(ms), last);
		if (segment !== undefined) last = segment;
		return segment;
	};

	// No boundary track: the default 1s interval paces the segments.
	expect(align(0)).toBe(0);
	expect(align(300)).toBeUndefined();
	expect(align(900)).toBeUndefined();
	expect(align(1200)).toBe(1);
});

test("first boundary adopts a self-opened segment", () => {
	const segmenter = new Segmenter();

	// Audio starts first and self-opens segment 0; the video keyframe arriving moments later
	// adopts it (same number) instead of stranding an audio-only segment 0, then paces on.
	expect(segmenter.align(us(0), undefined)).toBe(0);
	expect(segmenter.boundary(us(30))).toBe(0);
	expect(segmenter.boundary(us(2030))).toBe(1);
	expect(segmenter.align(us(2100), 0)).toBe(1);
});

test("aligned producer joining late records the current segment", () => {
	const segmenter = new Segmenter();
	segmenter.boundary(us(0));
	segmenter.boundary(us(2000));

	// An audio track added mid-broadcast: its first group lands in segment 1, not 0.
	expect(segmenter.align(us(2050), undefined)).toBe(1);
});
