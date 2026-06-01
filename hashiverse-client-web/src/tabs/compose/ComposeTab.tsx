import type React from "react";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { TabHeader } from "../TabHeader.tsx";
import { ComposeEditor } from "./ComposeEditor.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

export const ComposeTab: React.FC<Props> = ({ hashiverse }) => {
	return (
		<div className="FullColumnChildAndParent" style={{ position: "relative" }}>
			<TabHeader />
			<ComposeEditor hashiverse={hashiverse} restore_draft={true} />
		</div>
	);
};
