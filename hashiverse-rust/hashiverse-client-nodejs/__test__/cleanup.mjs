import { rmSync } from "node:fs";

/**
 * Tmpdir cleanup that releases native handles before deleting.
 *
 * SQLite (used by the napi-rs client's storage backend) keeps file handles
 * open until the Rust struct is dropped, which happens via V8's GC finalizer
 * for the napi-rs external — not when the test-scope variable goes out of
 * scope. On Linux/macOS the unlink succeeds anyway; on Windows it hits EPERM.
 *
 * Forcing a major GC via `globalThis.gc()` (exposed by `--expose-gc`, set up
 * in `vitest.config.mjs`) collects the client, but since Node 26 the napi-rs
 * finalizer that drops the Arc (and so closes the SQLite connections) is
 * deferred to a later event-loop turn rather than run inside `gc()`.  So we
 * yield to the event loop after each GC before trying to delete.
 *
 * We still keep the EPERM-tolerant fallback for safety — if `--expose-gc`
 * isn't wired up for some reason, or if a stray reference defeats GC, the
 * test won't fail; the OS will reclaim the tmp dir on its own.  Its retries
 * must stay well inside vitest's 10s hookTimeout (rmSync backs off linearly:
 * 5 retries x 100ms ~= 1.5s in total).
 */
export async function cleanupTmpDir(dir) {
    if (typeof globalThis.gc === "function") {
        for (let i = 0; i < 3; i++) {
            globalThis.gc();
            await new Promise((resolve) => setImmediate(resolve));
        }
    }
    try {
        rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    } catch (e) {
        if (e.code === "EPERM" && process.platform === "win32") {
            console.warn(`Skipping rmSync of ${dir} on Windows (SQLite handles still held after GC): ${e.message}`);
            return;
        }
        throw e;
    }
}
