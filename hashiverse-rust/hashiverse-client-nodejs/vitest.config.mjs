import { defineConfig } from "vitest/config";

// `--expose-gc` makes `globalThis.gc()` callable inside test workers. We use
// it in `__test__/cleanup.mjs` to force the napi-rs client to be finalised
// before deleting its tmp data directory — on Windows the SQLite handles are
// held until V8 GC runs, so without this rmSync hits EPERM.
export default defineConfig({
    test: {
        // vitest 4+ replaced per-pool `poolOptions` with a top-level `execArgv` that applies to every pool
        execArgv: ["--expose-gc"],
    },
});
