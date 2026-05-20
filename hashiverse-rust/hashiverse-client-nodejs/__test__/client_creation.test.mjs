import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, existsSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { HashiverseClient } from "../index.js";
import { cleanupTmpDir } from "./cleanup.mjs";

const BOOTSTRAP = ["127.0.0.1:19999"];

function makeClient(dataDir, keyPhrase = "test keyphrase", passphrase = "") {
    return HashiverseClient.createFromKeyphrase({
        keyPhrase,
        dataDir,
        passphrase,
        bootstrapAddresses: BOOTSTRAP,
    });
}

describe("client creation", () => {
    let parentDir;

    beforeEach(() => {
        parentDir = mkdtempSync(join(tmpdir(), "hashiverse-client-creation-"));
    });

    afterEach(() => {
        cleanupTmpDir(parentDir);
    });

    it("createFromKeyphrase returns a client", async () => {
        const client = await makeClient(join(parentDir, "data"));
        expect(client).toBeDefined();
    });

    it("clientId is a hex string", async () => {
        const client = await makeClient(join(parentDir, "data"));
        const clientId = client.clientId;
        expect(typeof clientId).toBe("string");
        expect(clientId.length).toBeGreaterThan(0);
        expect(/^[0-9a-f]+$/.test(clientId)).toBe(true);
    });

    it("loggedIn is true when keyPhrase provided", async () => {
        const client = await makeClient(join(parentDir, "data"), "some keyphrase");
        expect(client.loggedIn).toBe(true);
    });

    it("loggedIn is false when keyPhrase empty", async () => {
        const client = await makeClient(join(parentDir, "data"), "");
        expect(client.loggedIn).toBe(false);
    });

    it("same keyPhrase produces same clientId", async () => {
        const a = await makeClient(join(parentDir, "a"), "deterministic keyphrase");
        const b = await makeClient(join(parentDir, "b"), "deterministic keyphrase");
        expect(b.clientId).toBe(a.clientId);
    });

    it("different keyPhrases produce different clientIds", async () => {
        const a = await makeClient(join(parentDir, "a"), "keyphrase alpha");
        const b = await makeClient(join(parentDir, "b"), "keyphrase bravo");
        expect(b.clientId).not.toBe(a.clientId);
    });

    it("dataDir is created", async () => {
        const dataDir = join(parentDir, "data");
        expect(existsSync(dataDir)).toBe(false);
        await makeClient(dataDir);
        expect(statSync(dataDir).isDirectory()).toBe(true);
    });
});
