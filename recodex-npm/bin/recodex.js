#!/usr/bin/env node
// recodex — launcher for the recodex Rust binary.
//
// On first run it downloads the prebuilt binary for this platform from the
// recodex GitHub release, caches it under ~/.recodex/bin/, and execs it. On
// later runs it launches the cached binary directly. No optionalDependencies
// or platform sub-packages are required.

import { spawn } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const VERSION = "0.1.1";
const GITHUB = "Kartvya69/recodex";

// process.platform/process.arch -> release asset + entry binary name.
const PLATFORM = {
  "linux-x64": {
    asset: "recodex-x86_64-unknown-linux-gnu.tar.gz",
    entry: "recodex",
  },
  "win32-x64": {
    asset: "recodex-x86_64-pc-windows-msvc.zip",
    entry: "recodex.exe",
  },
};

function fail(msg, suggestion) {
  console.error(`recodex: ${msg}`);
  if (suggestion) console.error(suggestion);
  process.exit(1);
}

function cacheDir() {
  const home = process.env.HOME || process.env.USERPROFILE || tmpdir();
  return path.join(home, ".recodex", "bin");
}

async function ensureBinary() {
  const key = `${process.platform}-${process.arch}`;
  const p = PLATFORM[key];
  if (!p) {
    fail(
      `no prebuilt binary for ${process.platform}/${process.arch} in v${VERSION}.`,
      `Browse https://github.com/${GITHUB}/releases for available assets, or build from source.`,
    );
  }

  const dir = cacheDir();
  const cached = path.join(dir, `recodex-${VERSION}-${p.entry}`);
  if (existsSync(cached)) return cached;

  mkdirSync(dir, { recursive: true });

  const url = `https://github.com/${GITHUB}/releases/download/v${VERSION}/${p.asset}`;
  process.stderr.write(`recodex: downloading v${VERSION} (${key}) — one-time setup...\n`);

  let res;
  try {
    res = await fetch(url, { redirect: "follow" });
  } catch (err) {
    fail(`network error fetching ${url}: ${err && err.message ? err.message : err}`);
  }
  if (!res.ok) fail(`download failed: HTTP ${res.status} for ${url}`);

  const buf = Buffer.from(await res.arrayBuffer());

  if (process.platform === "win32") {
    // Windows: write the zip to a temp file and Expand-Archive it (always
    // available on Windows; avoids depending on a system `tar`).
    const tmpZip = path.join(dir, `__recodex-${VERSION}-${process.pid}.zip`);
    writeFileSync(tmpZip, buf);
    const code = await new Promise((resolve) => {
      const t = spawn(
        "powershell",
        ["-NoProfile", "-Command", `Expand-Archive -Path '${tmpZip}' -DestinationPath '${dir}' -Force`],
        { stdio: "inherit" },
      );
      t.on("exit", resolve);
      t.on("error", (err) => {
        console.error(`recodex: Expand-Archive failed: ${err.message}`);
        resolve(1);
      });
    });
    try { unlinkSync(tmpZip); } catch { /* ignore */ }
    if (code !== 0) fail("failed to extract the downloaded zip (Expand-Archive)");
  } else {
    // POSIX: stream the gzip tarball into tar.
    const code = await new Promise((resolve) => {
      const t = spawn("tar", ["-xz", "-C", dir, p.entry], {
        stdio: ["pipe", "inherit", "inherit"],
      });
      t.on("exit", resolve);
      t.on("error", (err) => {
        console.error(`recodex: tar extraction failed: ${err.message}`);
        resolve(1);
      });
      t.stdin.end(buf);
    });
    if (code !== 0) fail("failed to extract the downloaded tarball");
  }

  const extracted = path.join(dir, p.entry);
  if (!existsSync(extracted)) fail("extracted binary not found after extraction");
  renameSync(extracted, cached);
  try { chmodSync(cached, 0o755); } catch { /* Windows: chmod is a no-op */ }
  return cached;
}

const binaryPath = await ensureBinary();

const child = spawn(binaryPath, process.argv.slice(2), { stdio: "inherit" });

child.on("error", (err) => {
  console.error(`recodex: failed to launch binary: ${err.message}`);
  process.exit(1);
});

const forwardSignal = (signal) => {
  if (child.killed) return;
  try {
    child.kill(signal);
  } catch {
    /* ignore */
  }
};
["SIGINT", "SIGTERM", "SIGHUP"].forEach((sig) =>
  process.on(sig, () => forwardSignal(sig)),
);

const result = await new Promise((resolve) => {
  child.on("exit", (code, signal) => {
    if (signal) resolve({ type: "signal", signal });
    else resolve({ type: "code", exitCode: code ?? 1 });
  });
});

if (result.type === "signal") {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.exitCode);
}
