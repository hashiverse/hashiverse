import { Button, Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate } from "react-router";
import banner_not_logged_in from "../../media/banner_not_logged_in.svg";
import { redirect_to_login_with_return } from "../../tools/PostLoginRedirect.ts";
import { Banner } from "../timeline/Banner.tsx";

interface Props {
	message?: string;
}

export const NotLoggedInPanel: React.FC<Props> = ({ message }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const location = useLocation();

	return (
		<Banner
			image={<img src={banner_not_logged_in} alt="Not logged in" />}
			heading={
				<Text size="xl" fw={700}>
					{t("banner_not_logged_in.title")}
				</Text>
			}
			detail={
				<>
					<Text>{message ?? t("banner_not_logged_in.message")}</Text>
					<Button mt="sm" onClick={() => redirect_to_login_with_return(navigate, location.pathname + location.search)}>
						{t("banner_not_logged_in.log_in")}
					</Button>
				</>
			}
		/>
	);
};
