import type React from "react";

const prerelease_tag = process.env.PRERELEASE_TAG ?? "";

const banner_style: React.CSSProperties = {
	position: "fixed",
	top: "0.75rem",
	right: "0.75rem",
	zIndex: 9999,
	background: "#c87820",
	color: "#221100",
	fontFamily: "system-ui, -apple-system, sans-serif",
	fontWeight: 600,
	fontSize: "0.7rem",
	padding: "0.15rem 0.55rem",
	letterSpacing: "0.02em",
	borderRadius: "3px",
	boxShadow: "0 2px 6px rgba(0, 0, 0, 0.4)",
	pointerEvents: "none",
};

export const PrereleaseBanner: React.FC = () => {
	if (!prerelease_tag) return null;
	return <div style={banner_style}>Prerelease: {prerelease_tag}</div>;
};
