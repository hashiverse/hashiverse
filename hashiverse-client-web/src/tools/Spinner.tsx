import type React from "react";
import image_busy from "../media/busy.webp";

interface Props {
	size?: number;
	style?: React.CSSProperties;
}

export const Spinner: React.FC<Props> = ({ size = 32, style }) => <img src={image_busy} width={size} style={style} alt="Spinner" aria-hidden="true" />;
