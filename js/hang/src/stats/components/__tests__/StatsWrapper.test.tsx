import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { render } from "solid-js/web";
import { StatsWrapper } from "../StatsWrapper";

describe("StatsWrapper", () => {
	let container: HTMLDivElement;

	beforeEach(() => {
		container = document.createElement("div");
		document.body.appendChild(container);
	});

	afterEach(() => {
		document.body.removeChild(container);
	});

	it("renders with wrapper class", () => {
		render(() => <StatsWrapper />, container);

		const wrapper = container.querySelector(".stats__wrapper");
		expect(wrapper).toBeTruthy();
	});

	it("renders button component", () => {
		render(() => <StatsWrapper />, container);

		const button = container.querySelector(".stats__button");
		expect(button).toBeTruthy();
	});

	it("initially hides stats panel", () => {
		render(() => <StatsWrapper />, container);

		const panel = container.querySelector(".stats__panel");
		expect(panel).toBeFalsy();
	});

	it("has button with correct aria attributes", () => {
		render(() => <StatsWrapper />, container);

		const button = container.querySelector("button");
		expect(button?.getAttribute("aria-pressed")).toBe("false");
		expect(button?.getAttribute("aria-label")).toBe("Show stats");
	});

	it("button has correct accessibility attributes", () => {
		render(() => <StatsWrapper />, container);

		const button = container.querySelector("button");
		expect(button?.hasAttribute("aria-label")).toBe(true);
		expect(button?.hasAttribute("aria-pressed")).toBe(true);
	});

	it("renders with correct structure", () => {
		render(() => <StatsWrapper />, container);

		const wrapper = container.querySelector(".stats__wrapper");
		const button = wrapper?.querySelector(".stats__button");

		expect(wrapper).toBeTruthy();
		expect(button).toBeTruthy();
	});

	it("button is clickable", () => {
		render(() => <StatsWrapper />, container);

		const button = container.querySelector("button") as HTMLElement;
		expect(() => button.click()).not.toThrow();
	});
});
