import { expect, test } from "bun:test";
import { TRACK, userFields } from "./metadata.ts";

test("userFields seeds from values", () => {
	const user = userFields({ id: "a", name: "Ada", avatar: "ada.png" });
	expect(user.id.peek()).toBe("a");
	expect(user.name.peek()).toBe("Ada");
	expect(user.avatar.peek()).toBe("ada.png");
	expect(user.color.peek()).toBeUndefined();
});

test("core tracks are hang/*.json and extras share the section", () => {
	expect(TRACK.user).toBe("hang/user.json");
	expect(TRACK.preview).toBe("hang/preview.json");
	expect(TRACK.chat).toBe("hang/chat.json");
	expect(TRACK.location).toBe("hang/location.json");
});
