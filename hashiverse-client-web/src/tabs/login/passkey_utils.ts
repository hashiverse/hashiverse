// WebAuthn PRF-based passkey login for Hashiverse.
//
// The PRF (Pseudo-Random Function) extension returns a deterministic 32-byte value
// for a given credential + input. We use a fixed "magic phrase" as the input so the
// output is always the same for the same passkey — which we then treat as the
// keyphrase for normal Hashiverse key derivation.
//
// Passkeys are created as discoverable (resident) credentials, so the browser can
// enumerate them by RP ID without needing a stored credential ID — passing an empty
// allowCredentials list prompts the browser's built-in passkey picker.
//
// Synced passkeys (iCloud Keychain, Google Password Manager) replicate across devices,
// giving the user the same Hashiverse identity everywhere their passkey is available.

const MAGIC_PHRASE = "hashiverse-passkey-v1";

// PRF extension types are not yet in the standard TypeScript lib
interface PrfExtensionInput {
	prf?: { eval?: { first: ArrayBuffer } };
}

interface PrfExtensionResult {
	prf?: { enabled?: boolean; results?: { first?: ArrayBuffer } };
}

async function getMagicBytes(): Promise<ArrayBuffer> {
	return crypto.subtle.digest("SHA-256", new TextEncoder().encode(MAGIC_PHRASE));
}

export async function isPlatformAuthenticatorAvailable(): Promise<boolean> {
	if (typeof window.PublicKeyCredential === "undefined") return false;
	try {
		return await PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable();
	} catch {
		return false;
	}
}

function prfOutputToHex(prf_bytes: ArrayBuffer): string {
	return Array.from(new Uint8Array(prf_bytes))
		.map((b) => b.toString(16).padStart(2, "0"))
		.join("");
}

async function getPrfOutput(): Promise<string> {
	const magic_bytes = await getMagicBytes();

	const assertion = (await navigator.credentials.get({
		publicKey: {
			challenge: crypto.getRandomValues(new Uint8Array(32)),
			allowCredentials: [], // discoverable — browser shows its passkey picker
			userVerification: "preferred",
			extensions: {
				prf: { eval: { first: magic_bytes } },
			} as AuthenticationExtensionsClientInputs & PrfExtensionInput,
		},
	})) as PublicKeyCredential | null;

	if (!assertion) throw new Error("Passkey authentication was cancelled.");

	const extensions = assertion.getClientExtensionResults() as PrfExtensionResult;
	const prf_result = extensions.prf?.results?.first;
	if (!prf_result) {
		throw new Error("This device or browser does not support the PRF extension needed for passkey login. " + "Try Chrome 116+, Edge 116+, or Safari 17.4+.");
	}

	return prfOutputToHex(prf_result);
}

export async function createPasskeyAndGetPrf(): Promise<string> {
	const rp_id = window.location.hostname;

	const credential = (await navigator.credentials.create({
		publicKey: {
			challenge: crypto.getRandomValues(new Uint8Array(32)),
			rp: { id: rp_id, name: "Hashiverse" },
			user: {
				// All Hashiverse users share the same user record — the identity is determined
				// entirely by the PRF output, not by the WebAuthn user handle.
				id: new TextEncoder().encode("hashiverse-user"),
				name: "hashiverse",
				displayName: "Hashiverse",
			},
			pubKeyCredParams: [
				{ type: "public-key", alg: -7 }, // ES256
				{ type: "public-key", alg: -257 }, // RS256
			],
			authenticatorSelection: {
				residentKey: "preferred",
				userVerification: "preferred",
			},
			extensions: { prf: {} } as AuthenticationExtensionsClientInputs & PrfExtensionInput,
		},
	})) as PublicKeyCredential | null;

	if (!credential) throw new Error("Passkey creation was cancelled.");

	return getPrfOutput();
}

export async function signInWithPasskeyAndGetPrf(): Promise<string> {
	return getPrfOutput();
}
