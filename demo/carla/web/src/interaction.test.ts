import { describe, expect, test } from "bun:test";
import { focusFromHash, hashWithFocus, Interaction, ownsKey } from "./interaction.ts";

describe("Interaction", () => {
	test("focuses a truck without taking control", () => {
		const interaction = new Interaction();
		interaction.focus("truck1/front");
		expect(interaction.focused.peek()).toBe("truck1/front");
		expect(interaction.driving.peek()).toBeUndefined();
	});

	test("only drive intent for the focused truck takes control", () => {
		const interaction = new Interaction();
		expect(interaction.takeDrive("truck1/front")).toBe(false);
		expect(interaction.driving.peek()).toBeUndefined();

		interaction.focus("truck1/front");
		expect(interaction.takeDrive("truck1/front")).toBe(true);
		expect(interaction.driving.peek()).toBe("truck1/front");
	});

	test("the control button toggles the focused truck", () => {
		const interaction = new Interaction("truck1/front");
		interaction.toggleDrive("truck1/front");
		expect(interaction.driving.peek()).toBe("truck1/front");
		interaction.toggleDrive("truck1/front");
		expect(interaction.driving.peek()).toBeUndefined();
	});

	test("release keeps focus so a second escape can leave the view", () => {
		const interaction = new Interaction("truck1/front");
		interaction.takeDrive("truck1/front");
		expect(interaction.releaseDrive()).toBe(true);
		expect(interaction.driving.peek()).toBeUndefined();
		expect(interaction.focused.peek()).toBe("truck1/front");
		expect(interaction.releaseDrive()).toBe(false);
	});

	test("releases control when focus is left or changed", () => {
		const interaction = new Interaction("truck1/front");
		interaction.toggleDrive("truck1/front");
		interaction.focus("truck2/front");
		expect(interaction.driving.peek()).toBeUndefined();

		interaction.toggleDrive("truck2/front");
		interaction.focus(undefined);
		expect(interaction.driving.peek()).toBeUndefined();
	});

	test("clears control when an unfocused truck disappears", () => {
		const interaction = new Interaction();
		// Exercise cleanup independently of focus, as if navigation already moved.
		interaction.driving.set("truck1/front");
		expect(interaction.remove("truck1/front")).toBe(false);
		expect(interaction.driving.peek()).toBeUndefined();
	});

	test("targets a focused viewer's truck for modal e-stop", () => {
		const interaction = new Interaction("truck1/front");
		expect(interaction.driving.peek()).toBeUndefined();
		expect(interaction.safetyTarget()).toBe("truck1/front");
	});
});

describe("focus URL state", () => {
	test("round trips a broadcast id", () => {
		const hash = hashWithFocus("", "truck1/front");
		expect(hash).toBe("#truck=truck1%2Ffront");
		expect(focusFromHash(hash)).toBe("truck1/front");
	});

	test("preserves unrelated fragment parameters", () => {
		expect(hashWithFocus("#other=1", "truck2/front")).toBe("#other=1&truck=truck2%2Ffront");
		expect(hashWithFocus("#other=1&truck=truck2%2Ffront", undefined)).toBe("#other=1");
	});

	test("treats an empty focus as preview", () => {
		expect(focusFromHash("#truck=")).toBeUndefined();
		expect(hashWithFocus("#truck=truck1%2Ffront", undefined)).toBe("");
	});
});

test("hover cannot claim control and duplicate URL focus does not erase a keyboard preview", () => {
	const state = new Interaction();
	state.hovered.set("truck1/front");
	state.focus(undefined);
	expect(state.hovered.peek()).toBe("truck1/front");
	expect(state.driving.peek()).toBeUndefined();
	state.focus("truck2/front");
	expect(state.hovered.peek()).toBeUndefined();
	expect(state.driving.peek()).toBeUndefined();
	state.hovered.set("truck1/front");
	state.remove("truck1/front");
	expect(state.hovered.peek()).toBeUndefined();
});

test("a retracted truck loses preview, focus, and control before re-announcement", () => {
	const state = new Interaction("truck1/front");
	state.takeDrive("truck1/front");
	state.hovered.set("truck1/front");
	expect(state.remove("truck1/front")).toBe(true);
	expect(state.focused.peek()).toBeUndefined();
	expect(state.driving.peek()).toBeUndefined();
	expect(state.hovered.peek()).toBeUndefined();
	state.focus("truck1/front");
	expect(state.driving.peek()).toBeUndefined();
});

describe("ownsKey", () => {
	// Stand-ins for the DOM nodes, which bun's test runtime doesn't provide.
	const node = () => ({ nodeType: 1 }) as unknown as Node;
	const body = node();
	const documentElement = node();
	const host = node();
	const inside = node();
	const root = {
		host,
		ownerDocument: { body, documentElement },
		contains: (other: Node | null) => other === inside,
	} as unknown as ShadowRoot;

	test("takes keys aimed inside the player or at nothing in particular", () => {
		expect(ownsKey(inside, root)).toBe(true);
		expect(ownsKey(host, root)).toBe(true);
		expect(ownsKey(body, root)).toBe(true);
		expect(ownsKey(documentElement, root)).toBe(true);
	});

	test("leaves keys aimed at the host page's own controls", () => {
		expect(ownsKey(node(), root)).toBe(false);
	});
});
