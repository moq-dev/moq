/**
 * Props for toggle button component
 */
interface ButtonProps {
	/** Whether stats panel is currently visible */
	isVisible: boolean;
	/** Callback when button is clicked */
	onToggle: (value: boolean) => void;
	/** SVG icon markup */
	icon: string;
}

/**
 * Toggle button for showing/hiding stats panel
 */
export const Button = (props: ButtonProps) => {
    return (
        <button
            class="stats__button"
            onClick={() => props.onToggle(!props.isVisible)}
            title={props.isVisible ? "Hide stats" : "Show stats"}
            aria-label={props.isVisible ? "Hide stats" : "Show stats"}
            aria-pressed={props.isVisible}
        >
            <div class="stats__icon" innerHTML={props.icon} />
        </button>
    );
};
