import type { NavigateFunction } from "react-router";
import { LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN, local_settings_set } from "./LocalSettings.ts";

export function redirect_to_login_with_return(navigate: NavigateFunction, return_url: string, options?: { replace?: boolean }): void {
	local_settings_set(LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN, return_url).catch(console.error);
	navigate("/login", options);
}
