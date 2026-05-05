import { Alert, Anchor, Group } from "@mantine/core";
import { IconWorldWww } from "@tabler/icons-react";
import type React from "react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { get_alternate_domain_urls } from "../tabs/login/domains.ts";
import { LOCAL_SETTINGS_KEY_PREFERRED_DOMAIN, local_settings_set } from "./LocalSettings.ts";

interface Props {
	hash_path: string;
}

export const DomainSwitcherBanner: React.FC<Props> = ({ hash_path }) => {
	const { t } = useTranslation();
	const [dismissed, dismissed_set] = useState(false);

	if (dismissed) return null;

	const alternate_domain_urls = get_alternate_domain_urls(hash_path);

	return (
		<Alert variant="light" color="blue" icon={<IconWorldWww />} withCloseButton onClose={() => dismissed_set(true)}>
			{t("domain_switcher.not_logged_in_hint", {
				domain: window.location.hostname,
			})}{" "}
			<Group component="span" gap="xs" display="inline-flex">
				{alternate_domain_urls.map(({ display_name, hostname, url }) => (
					<Anchor
						key={display_name}
						href={url}
						fw={600}
						onClick={(event) => {
							if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
							event.preventDefault();
							local_settings_set(LOCAL_SETTINGS_KEY_PREFERRED_DOMAIN, hostname)
								.catch(console.error)
								.finally(() => {
									window.location.href = url;
								});
						}}
					>
						{display_name}
					</Anchor>
				))}
			</Group>
		</Alert>
	);
};
