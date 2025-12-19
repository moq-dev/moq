import "@testing-library/jest-dom";
import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import Button, { type ButtonProps } from "../../components/button/index";

describe("Button component", () => {
	const defaultProps: ButtonProps = {
		children: "Click me",
	};

	it("renders with default props", () => {
		const { getByRole } = render(() => <Button {...defaultProps} />);
		const button = getByRole("button");
		expect(button).toBeInTheDocument();
		expect(button).toHaveTextContent("Click me");
		expect(button).toHaveAttribute("type", "button");
		expect(button).toHaveAttribute("title", "Simple button");
		expect(button).toHaveClass("button");
	});

	it("applies custom class and title", () => {
		const { getByRole } = render(() => <Button {...defaultProps} class="custom" title="My Button" />);
		const button = getByRole("button");
		expect(button).toHaveClass("custom");
		expect(button).toHaveAttribute("title", "My Button");
	});

	it("handles click events", () => {
		const onClick = vi.fn();
		const { getByRole } = render(() => <Button {...defaultProps} onClick={onClick} />);
		const button = getByRole("button");
		fireEvent.click(button);
		expect(onClick).toHaveBeenCalledTimes(1);
	});

	it("sets aria-label and aria-disabled", () => {
		const { getByRole } = render(() => <Button {...defaultProps} ariaLabel="label" ariaDisabled />);
		const button = getByRole("button");
		expect(button).toHaveAttribute("aria-label", "label");
		expect(button).toHaveAttribute("aria-disabled", "true");
	});

	it("sets disabled and tabIndex", () => {
		const { getByRole } = render(() => <Button {...defaultProps} disabled tabIndex={-1} />);
		const button = getByRole("button");
		expect(button).toBeDisabled();
		expect(button).toHaveAttribute("tabindex", "-1");
	});

	it("renders JSX children", () => {
		const { getByRole } = render(() => (
			<Button>
				{" "}
				<span data-testid="icon">Icon</span>{" "}
			</Button>
		));
		const button = getByRole("button");
		expect(button.querySelector('[data-testid="icon"]')).toBeInTheDocument();
	});
});
