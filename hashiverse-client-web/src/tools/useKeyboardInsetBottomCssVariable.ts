import { useEffect } from "react";

// iOS Safari does not shrink the layout viewport when the on-screen keyboard appears;
// only the visual viewport shrinks. Mirror that gap into a CSS variable so absolutely
// positioned overlays (e.g. the compose submit button) can dodge the keyboard via
// `bottom: calc(... + var(--keyboard-inset-bottom, 0px))`. On Android/desktop where
// the layout viewport itself resizes, the value stays at 0 and the calc is a no-op.
export function useKeyboardInsetBottomCssVariable(): void {
	useEffect(() => {
		const visual_viewport = window.visualViewport;
		if (!visual_viewport) return;

		const update = () => {
			const inset_bottom_px = Math.max(0, window.innerHeight - visual_viewport.height - visual_viewport.offsetTop);
			document.documentElement.style.setProperty("--keyboard-inset-bottom", `${inset_bottom_px}px`);
		};

		update();
		visual_viewport.addEventListener("resize", update);
		visual_viewport.addEventListener("scroll", update);
		return () => {
			visual_viewport.removeEventListener("resize", update);
			visual_viewport.removeEventListener("scroll", update);
			document.documentElement.style.removeProperty("--keyboard-inset-bottom");
		};
	}, []);
}
