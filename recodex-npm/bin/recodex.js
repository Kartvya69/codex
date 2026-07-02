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
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const VERSION = "0.1.0";
const GITHUB = "Kartvya69/recodex";

// Map process.platform/process.arch -> the release asset triple.
// v0.1.0 ships Linux x86_64 (glibc). Other platforms will be added in later
// releases; until then we fail fast with a helpful message.
const ASSET_BY_PLATFORM = {
  "linux-x64": "recodex-x86_64-unknown-linux-gnu.tar.gz",
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
  const asset = ASSET_BY_PLATFORM[key];
  if (!asset) {
    fail(
      `no prebuilt binary for ${process.platform}/${process.arch} in v${VERSION}.`,
      `Browse https://github.com/${GITHUB}/releases for available assets, or build from source.`,
    );
  }

  const dir = cacheDir();
  const cached = path.join(dir, `recodex-${VERSION}`);
  if (existsSync(cached)) return cached;

  mkdirSync(dir, { recursive: true });

  const url = `https://github.com/${GITHUB}/releases/download/v${VERSION}/${asset}`;
  process.stderr.write(`recodex: downloading v${VERSION} (${key}) — one-time setup...\n`);

  let res;
  try {
    res = await fetch(url, { redirect: "follow" });
  } catch (err) {
    fail(`network error fetching ${url}: ${err && err.message ? err.message : err}`);
  }
  if (!res.ok) fail(`download failed: HTTP ${res.status} for ${url}`);

  const buf = Buffer.from(await res.arrayBuffer());
  const code = await new Promise((resolve) => {
    const t = spawn("tar", ["-xz", "-C", dir, "recodex"], {
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

  const extracted = path.join(dir, "recodex");
  if (!existsSync(extracted)) fail("extracted binary not found after untar");
  renameSync(extracted, cached);
  chmodSync(cached, 0o755);
  return cached;
}

const binaryPath = await ensureBinary();

const env = {
  ...process.env,
  CODEX_MANAGED_PACKAGE_ROOT: undefined,
};

const child = spawn(binaryPath, process.argv.slice(2), { stdio: "inherit", env });

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
