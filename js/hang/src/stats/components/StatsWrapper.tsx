import { createSignal, Show } from "solid-js";
import { Button } from "./Button";
import { StatsPanel } from "./StatsPanel";
import { useMetrics } from "../context";
import { BUTTON_SVG } from "./icons";

export const StatsWrapper = () => {
    const [isVisible, setIsVisible] = createSignal(false);
    const metrics = useMetrics();

    return (
        <div class="stats__wrapper">
            <Button isVisible={isVisible()} onToggle={setIsVisible} icon={BUTTON_SVG} />

            <Show when={isVisible()}>
                <StatsPanel audio={metrics.audio} video={metrics.video} />
            </Show>
        </div>
    );
};
