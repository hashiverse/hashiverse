import type React from "react";
import { useTranslation } from "react-i18next";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";

export const AcknowledgementsSection: React.FC = () => {
	const { t } = useTranslation();
	return (
		<CollapsiblePanel title={t("acknowledgements.title")} defaultOpen={false}>
			<ul style={{ margin: 0, paddingLeft: 16 }}>
				<li>
					Vector illustrations by Storyset <a href="https://storyset.com/data">🔗</a>
				</li>
				<li>
					Confirm Jingle sound by JustInvoke <a href="https://freesound.org/s/446114/">🔗</a>
				</li>
				<li>
					Cancel sound by Feraly <a href="https://freesound.org/s/836449/">🔗</a>
				</li>
				<li>
					Confirm sound by Feraly <a href="https://freesound.org/s/836450/">🔗</a>
				</li>
			</ul>
		</CollapsiblePanel>
	);
};
