import type React from "react";

interface BannerProps {
	heading: React.ReactNode;
	detail?: React.ReactNode;
	image?: React.ReactNode;
}

export const Banner: React.FC<BannerProps> = ({ heading, detail, image }) => (
	<div
		style={{
			padding: "12px",
			overflow: "hidden",
			background: "rgba(0,0,0,0.25)",
		}}
	>
		{image && (
			<div
				className="BannerImage"
				style={{
					float: "right",
					position: "relative",
					maxWidth: "30%",
					marginLeft: "12px",
				}}
			>
				{image}
			</div>
		)}
		{heading}
		{detail}
	</div>
);
