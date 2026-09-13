import { expect, test } from "bun:test";
import { Path } from "@moq/net";
import { broadcastPath, isKind, KIND, kindFromSegment, parse } from "./path.ts";

const p = (s: string) => Path.from(s);

test("parse splits identity and kind", () => {
	expect(parse(p("alice/camera"))).toEqual({ identity: p("alice"), kind: "camera" });
	expect(parse(p("alice/screen"))).toEqual({ identity: p("alice"), kind: "screen" });
});

test("parse accepts a .hang suffix on the kind", () => {
	expect(parse(p("alice/camera.hang"))).toEqual({ identity: p("alice"), kind: "camera" });
	expect(parse(p("alice/screen.hang"))).toEqual({ identity: p("alice"), kind: "screen" });
});

test("parse keeps a multi-segment identity", () => {
	expect(parse(p("guest/uuid/camera"))).toEqual({ identity: p("guest/uuid"), kind: "camera" });
});

test("parse rejects a path with no kind or no identity", () => {
	expect(parse(p("alice"))).toBeUndefined();
	expect(parse(p("alice/chat"))).toBeUndefined();
	expect(parse(p("camera"))).toBeUndefined();
	expect(parse(p(""))).toBeUndefined();
});

test("kindFromSegment and isKind", () => {
	expect(kindFromSegment("camera")).toBe("camera");
	expect(kindFromSegment("camera.hang")).toBe("camera");
	expect(kindFromSegment("chat")).toBeUndefined();
	expect(isKind("camera")).toBe(true);
	expect(isKind("screen")).toBe(true);
	expect(isKind("chat")).toBe(false);
});

test("broadcastPath joins identity and kind with a .hang suffix", () => {
	expect(broadcastPath(p("alice"), KIND.camera)).toBe(p("alice/camera.hang"));
	expect(broadcastPath(p("guest/uuid"), KIND.screen)).toBe(p("guest/uuid/screen.hang"));
});
