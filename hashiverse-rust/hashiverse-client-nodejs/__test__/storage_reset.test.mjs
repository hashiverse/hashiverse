import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HashiverseClient } from "../index.js";
import { cleanupTmpDir } from "./cleanup.mjs";

const BOOTSTRAP = ["127.0.0.1:19999"];

describe("storage reset", () => {
    let dataDir;

    beforeEach(() => {
        dataDir = mkdtempSync(join(tmpdir(), "hashiverse-storage-reset-"));
    });

    afterEach(() => {
        cleanupTmpDir(dataDir);
    });

    it("resetStorage does not throw", async () => {
        const client = await HashiverseClient.createFromKeyphrase({
            keyPhrase: "x",
            dataDir,
            passphrase: "",
            bootstrapAddresses: BOOTSTRAP,
        });
        await client.resetStorage();
    });

    it("resetStorage keeps stored keys intact", async () => {
        const client = await HashiverseClient.createFromKeyphrase({
            keyPhrase: "persist after reset",
            dataDir,
            passphrase: "",
            bootstrapAddresses: BOOTSTRAP,
        });
        const clientId = client.clientId;
        await client.resetStorage();
        const storedKeys = await client.listStoredKeys();
        expect(storedKeys).toContain(clientId);
    });
});
