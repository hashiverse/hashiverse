import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HashiverseClient } from "../index.js";

const BOOTSTRAP = ["127.0.0.1:19999"];

function makeClient(dataDir, keyPhrase = "test keyphrase", passphrase = "") {
    return HashiverseClient.createFromKeyphrase({
        keyPhrase,
        dataDir,
        passphrase,
        bootstrapAddresses: BOOTSTRAP,
    });
}

describe("key storage", () => {
    let dataDir;

    beforeEach(() => {
        dataDir = mkdtempSync(join(tmpdir(), "hashiverse-key-storage-"));
    });

    afterEach(() => {
        // Windows holds SQLite file handles open until JS GC finalises the
        // napi-rs client, so rmSync can hit EPERM. With force: true, rmSync
        // retries on EPERM up to maxRetries with linear backoff.
        rmSync(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    });

    it("new client's key is listed", async () => {
        const client = await makeClient(dataDir, "stored key test");
        const storedKeys = await client.listStoredKeys();
        expect(storedKeys).toContain(client.clientId);
    });

    it("multiple keys are listed", async () => {
        const a = await makeClient(dataDir, "key alpha");
        const b = await makeClient(dataDir, "key bravo");
        const storedKeys = await b.listStoredKeys();
        expect(storedKeys).toContain(a.clientId);
        expect(storedKeys).toContain(b.clientId);
    });

    it("createFromStoredKey restores by clientId", async () => {
        const original = await makeClient(dataDir, "restore me");
        const restored = await HashiverseClient.createFromStoredKey({
            clientIdHex: original.clientId,
            dataDir,
            passphrase: "",
            bootstrapAddresses: BOOTSTRAP,
        });
        expect(restored.clientId).toBe(original.clientId);
        expect(restored.loggedIn).toBe(true);
    });

    it("createFromStoredKey honours the passphrase", async () => {
        const original = await makeClient(dataDir, "encrypted key", "s3cret");
        const restored = await HashiverseClient.createFromStoredKey({
            clientIdHex: original.clientId,
            dataDir,
            passphrase: "s3cret",
            bootstrapAddresses: BOOTSTRAP,
        });
        expect(restored.clientId).toBe(original.clientId);
    });

    it("deleteStoredKey removes a single key", async () => {
        const a = await makeClient(dataDir, "keep me");
        const b = await makeClient(dataDir, "delete me");
        const bId = b.clientId;
        await b.deleteStoredKey(bId);
        const storedKeys = await b.listStoredKeys();
        expect(storedKeys).not.toContain(bId);
        expect(storedKeys).toContain(a.clientId);
    });

    it("deleteAllStoredKeys wipes everything", async () => {
        await makeClient(dataDir, "one");
        const b = await makeClient(dataDir, "two");
        await b.deleteAllStoredKeys();
        const storedKeys = await b.listStoredKeys();
        expect(storedKeys).toHaveLength(0);
    });

    it("createFromStoredKey on a non-existent key throws", async () => {
        await makeClient(dataDir, "setup");
        const fakeHexId = "ab".repeat(32); // 64 hex chars
        await expect(
            HashiverseClient.createFromStoredKey({
                clientIdHex: fakeHexId,
                dataDir,
                passphrase: "",
                bootstrapAddresses: BOOTSTRAP,
            }),
        ).rejects.toThrow();
    });
});
