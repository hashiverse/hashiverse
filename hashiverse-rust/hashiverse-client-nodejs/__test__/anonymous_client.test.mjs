import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HashiverseClient } from "../index.js";

const BOOTSTRAP = ["127.0.0.1:19999"];

describe("anonymous client", () => {
    let dataDir;

    beforeEach(() => {
        dataDir = mkdtempSync(join(tmpdir(), "hashiverse-anon-"));
    });

    afterEach(() => {
        rmSync(dataDir, { recursive: true, force: true });
    });

    it("anonymous client has a clientId", async () => {
        const client = await HashiverseClient.createFromKeyphrase({
            keyPhrase: "",
            dataDir,
            passphrase: "",
            bootstrapAddresses: BOOTSTRAP,
        });
        expect(client.clientId.length).toBeGreaterThan(0);
    });

    it("anonymous client is not loggedIn", async () => {
        const client = await HashiverseClient.createFromKeyphrase({
            keyPhrase: "",
            dataDir,
            passphrase: "",
            bootstrapAddresses: BOOTSTRAP,
        });
        expect(client.loggedIn).toBe(false);
    });
});
