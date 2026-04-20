export type RPCRequest =
	| { type: "create_from_keyphrase"; keyPhrase: string }
	| { type: "create_from_stored_key"; keyPublic: string }
	| { type: "call"; method: string; args: unknown[] }
	| { type: "dispose" };

export type RPCResponse = { ok: true; result: unknown } | { ok: false; error: { message: string; name?: string; stack?: string } };
