/**
 * LocalSettings: device-local preferences that do NOT sync across devices.
 *
 * These are settings specific to this browser/machine (e.g. UI language,
 * bootstrap server, login state).  Settings that should sync across devices
 * belong in the MetaPostV1 config (via the Rust/WASM layer).
 *
 * @module
 */

const DB_NAME = "hashiverse.local_settings";
const DB_VERSION = 1;
const STORE_NAME = "settings";

// --- Keys ---
export const LOCAL_SETTINGS_KEY_LANGUAGE = "language";
export const LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY = "last_login_key";
export const LOCAL_SETTINGS_KEY_BOOTSTRAP = "bootstrap";
export const LOCAL_SETTINGS_KEY_DRAFT_POST = "hashiverse.draft_post";
export const LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN = "post_login_return";

function open_db(): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const request = indexedDB.open(DB_NAME, DB_VERSION);
		request.onupgradeneeded = (event) => {
			const db = (event.target as IDBOpenDBRequest).result;
			if (!db.objectStoreNames.contains(STORE_NAME)) {
				db.createObjectStore(STORE_NAME, { keyPath: "key" });
			}
		};
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error);
	});
}

export async function local_settings_get(key: string): Promise<string | null> {
	const db = await open_db();
	return new Promise((resolve, reject) => {
		const tx = db.transaction(STORE_NAME, "readonly");
		const store = tx.objectStore(STORE_NAME);
		const request = store.get(key);
		request.onsuccess = () => resolve(request.result?.value ?? null);
		request.onerror = () => reject(request.error);
	});
}

export async function local_settings_set(key: string, value: string): Promise<void> {
	const db = await open_db();
	return new Promise((resolve, reject) => {
		const tx = db.transaction(STORE_NAME, "readwrite");
		const store = tx.objectStore(STORE_NAME);
		const request = store.put({ key, value });
		request.onsuccess = () => resolve();
		request.onerror = () => reject(request.error);
	});
}

export async function local_settings_delete(key: string): Promise<void> {
	const db = await open_db();
	return new Promise((resolve, reject) => {
		const tx = db.transaction(STORE_NAME, "readwrite");
		const store = tx.objectStore(STORE_NAME);
		const request = store.delete(key);
		request.onsuccess = () => resolve();
		request.onerror = () => reject(request.error);
	});
}

export async function local_settings_reset(): Promise<void> {
	const db = await open_db();
	return new Promise((resolve, reject) => {
		const tx = db.transaction(STORE_NAME, "readwrite");
		const store = tx.objectStore(STORE_NAME);
		const request = store.clear();
		request.onsuccess = () => resolve();
		request.onerror = () => reject(request.error);
	});
}
