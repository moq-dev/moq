import { onMount } from "solid-js";
import type { JSX } from "solid-js/jsx-runtime";
import { useConstructableStyle } from "../../styles/utils";
import buttonStyle from "./style.css?inline";

const sheet = new CSSStyleSheet();
sheet.replaceSync(`${buttonStyle}`);

/**
 * Props for the shared Button component.
 *
 * @property {('button' | 'submit' | 'reset')} [type] - Button type attribute. Defaults to 'button'.
 * @property {string} [title] - Tooltip/title attribute for the button. Defaults to 'Simple button'.
 * @property {() => void} [onClick] - Click handler function.
 * @property {string} [class] - Additional CSS classes for custom styling.
 * @property {JSX.Element | string} children - Button content (JSX or string). Required.
 * @property {string} [ariaLabel] - Accessible label for screen readers. Falls back to title or children if not provided.
 * @property {boolean} [ariaDisabled] - Accessibility disabled state (for ARIA only).
 * @property {boolean} [disabled] - Disabled state (native HTML attribute).
 * @property {number} [tabIndex] - Tab index for keyboard navigation. Defaults to 0.
 */
export type ButtonProps = {
	type?: "button" | "submit" | "reset";
	title?: string;
	onClick?: () => void;
	class?: string;
	children: JSX.Element | string;
	ariaLabel?: string;
	ariaDisabled?: boolean;
	disabled?: boolean;
	tabIndex?: number;
};

/**
 * Shared, accessible, and stylable Button component for SolidJS.
 *
 * - Injects a Constructable Stylesheet for style encapsulation (Shadow DOM-friendly).
 * - Supports accessibility attributes and keyboard navigation.
 * - Accepts custom content, classes, and state modifiers (e.g., active, disabled).
 *
 * @param {ButtonProps} props - Button configuration and content.
 * @returns {JSX.Element} A styled, accessible button element.
 */
export default function Button(props: ButtonProps) {
	let buttonRef: HTMLButtonElement | undefined;

	onMount(() => {
		useConstructableStyle(buttonRef, sheet);
	});

	return (
		<button
			ref={buttonRef}
			type={props.type ?? "button"}
			title={props.title ?? "Simple button"}
			class={`flex center button ${props.class ? `${props.class}` : ""}`.trimEnd()}
			onClick={props.onClick}
			aria-label={
				props.ariaLabel ?? props.title ?? (typeof props.children === "string" ? props.children : undefined)
			}
			aria-disabled={props.ariaDisabled ?? props.disabled}
			disabled={props.disabled}
			tabIndex={props.tabIndex}
		>
			{props.children}
		</button>
	);
}
