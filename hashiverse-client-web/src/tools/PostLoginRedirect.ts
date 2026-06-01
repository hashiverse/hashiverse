import type { NavigateFunction } from "react-router";
import { LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN, local_settings_set } from "./LocalSettings.ts";

export async function redirect_to_login_with_return(navigate: NavigateFunction, return_url: string, options?: { replace?: boolean }): Promise<void> {
	try {
		await local_settings_set(LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN, return_url);
	} catch (error) {
		console.error(error);
	}
	navigate("/login", options);
}
