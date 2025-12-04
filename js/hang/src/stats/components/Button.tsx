interface ButtonProps {
    isVisible: boolean;
    onToggle: (value: boolean) => void;
    icon: string;
}

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
