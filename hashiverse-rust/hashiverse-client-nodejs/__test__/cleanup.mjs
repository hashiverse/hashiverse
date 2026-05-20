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
 * in `vitest.config.mjs`) runs the finalizer synchronously, which drops the
 * Arc and closes the SQLite connections before we try to delete.
 *
 * We still keep the EPERM-tolerant fallback for safety — if `--expose-gc`
 * isn't wired up for some reason, or if a stray reference defeats GC, the
 * test won't fail; the OS will reclaim the tmp dir on its own.
 */
export function cleanupTmpDir(dir) {
    if (typeof globalThis.gc === "function") {
        globalThis.gc();
    }
    try {
        rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    } catch (e) {
        if (e.code === "EPERM" && process.platform === "win32") {
            console.warn(`Skipping rmSync of ${dir} on Windows (SQLite handles still held after GC): ${e.message}`);
            return;
        }
        throw e;
    }
}
