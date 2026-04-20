import { Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import banner_error from "../media/banner_error.svg";
import { TabHeader } from "./TabHeader.tsx";
import { Banner } from "./timeline/Banner.tsx";

export const ErrorTab: React.FC = () => {
	const { t } = useTranslation();

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<Banner
				image={<img src={banner_error} alt="error" />}
				heading={
					<Text size="xl" fw={700}>
						{t("error.title")}
					</Text>
				}
				detail={<Text>{t("error.message")}</Text>}
			/>
		</div>
	);
};
