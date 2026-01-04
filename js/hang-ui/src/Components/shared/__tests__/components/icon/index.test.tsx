import "@testing-library/jest-dom";
import { render, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setBasePath } from "../../../../../utilities";
import Icon from "../../../../shared/components/icon";

describe("Icon component", () => {
	beforeEach(() => {
		// Set a fake base path for tests
		setBasePath("/test-assets");
	});

	it("renders and fetches SVG icon", async () => {
		const svgContent = `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"></svg>`;

		global.fetch = vi.fn().mockResolvedValue({
			ok: true,
			text: () => Promise.resolve(svgContent),
		});

		const { findByRole } = render(() => <Icon name="test" />);

		const span = await findByRole("img", { hidden: true });

		expect(span).toBeInTheDocument();

		await waitFor(() => {
			expect(span.innerHTML).toBe(svgContent);
			expect(span.dataset.iconLoading).toBeUndefined();
			expect(span.dataset.iconError).toBeUndefined();
		});
	});

	it("shows error state if fetch fails", async () => {
		global.fetch = vi.fn().mockResolvedValue({ ok: false });
		const { findByRole } = render(() => <Icon name="fail" />);

		const span = await findByRole("img", { hidden: true });

		expect(span).toBeInTheDocument();

		await waitFor(() => {
			expect(span.innerHTML).toBe("");
			expect(span.dataset.iconLoading).toBeUndefined();
			expect(span.dataset.iconError).toBe("true");
		});
	});

	it("caches icons and avoids duplicate fetches", async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			text: () => Promise.resolve(`<svg id="cached-icon"></svg>`),
		});

		global.fetch = fetchMock;

		const { getAllByRole } = render(() => (
			<>
				<Icon name="cached" />
				<Icon name="cached" />
			</>
		));

		const spans = getAllByRole("img", { hidden: true });

		expect(spans.length).toBe(2);

		await waitFor(() => {
			expect(spans[0].innerHTML).toContain(`<svg id="cached-icon"></svg>`);
			expect(spans[1].innerHTML).toContain(`<svg id="cached-icon"></svg>`);
		});
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});
});
