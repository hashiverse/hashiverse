import { Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import logo from "../../media/logo.webp";
import { TrendingHashtagsGrid } from "../hashtags/TrendingHashtagsGrid.tsx";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "../timeline/Banner.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

export const HomeTab: React.FC<Props> = ({ hashiverse }) => {
	const { t } = useTranslation();

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<div className="FullColumnChildScrollable">
				<Banner
					image={<img src={logo} alt="Hashiverse" />}
					heading={
						<Text size="xl" fw={700}>
							{t("home.banner_heading")}
						</Text>
					}
					detail={<Text>{t("home.banner_detail")}</Text>}
				/>
				<TrendingHashtagsGrid hashiverse={hashiverse} />
			</div>
		</div>
	);
};
