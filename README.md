<div align="center">

# recodex

**A community fork of [OpenAI Codex](https://github.com/openai/codex) that makes bring-your-own-key (BYOK) first-class.**

[![npm version](https://img.shields.io/npm/v/recodex?color=blue&label=npm)](https://www.npmjs.com/package/recodex)
[![GitHub release](https://img.shields.io/github/v/release/Kartvya69/recodex?color=blue&label=release)](https://github.com/Kartvya69/recodex/releases)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

</div>

`recodex` is a coding agent that runs locally in your terminal. It tracks upstream Codex closely — everything Codex does works here too — and adds two things that matter when you point it at **your own model provider and key**:

1. **A restored Chat Completions wire API** — use any OpenAI-compatible `/v1/chat/completions` endpoint (OpenRouter, ZAI, Together, Groq, Fireworks, Ollama, LM Studio, vLLM, …).
2. **Automatic [models.dev](https://models.dev) metadata enrichment** — unknown model slugs get real display names + context windows, cached on disk, so BYOK models behave correctly instead of falling back to a conservative default.

---

## Install

**npm** (recommended):

```shell
npm install -g recodex
recodex --version
```

The npm package is a thin launcher: on first run it downloads the prebuilt binary for your platform from the [latest release](https://github.com/Kartvya69/recodex/releases) and caches it under `~/.recodex/bin/`; later runs launch it directly.

**Prebuilt binary** — Linux x86_64:

```shell
curl -fsSL https://github.com/Kartvya69/recodex/releases/latest/download/recodex-x86_64-unknown-linux-gnu.tar.gz \
  | sudo tar -xz -C /usr/local/bin recodex
recodex --version
```

**Prebuilt binary** — Windows x86_64 (PowerShell):

```powershell
$dst = "$env:LOCALAPPDATA\recodex"
New-Item -ItemType Directory -Force -Path $dst | Out-Null
Invoke-WebRequest "https://github.com/Kartvya69/recodex/releases/latest/download/recodex-x86_64-pc-windows-msvc.zip" -OutFile "$dst\recodex.zip"
Expand-Archive "$dst\recodex.zip" -DestinationPath $dst -Force
Remove-Item "$dst\recodex.zip"
# Add $dst to your PATH, then:
recodex --version
```

<details>
<summary><b>Build from source</b></summary>

```shell
git clone https://github.com/Kartvya69/recodex.git
cd recodex/codex-rs
cargo build --release --bin recodex
# binary: target/release/recodex
```

Requires Rust 1.95 (pinned by `codex-rs/rust-toolchain.toml`) and `pkg-config` + `libssl-dev` + `libcap-dev` on Linux.

</details>

> **Note:** configuration lives in `~/.codex/` (same as upstream Codex; on Windows that's `%USERPROFILE%\.codex`). Only the command name is `recodex`. v0.1.1 ships **Linux x86_64 (glibc)** and **Windows x86_64** binaries; macOS and arm64 will follow.

---

## What's different from upstream Codex

### 1. Chat Completions is back — BYOK, natively

Upstream Codex deprecated the OpenAI **Chat Completions** wire protocol and standardized on the **Responses** API (`/v1/responses`). Most third-party providers and self-hosted servers only speak `/v1/chat/completions`, so `recodex` restores the full Chat Completions path and lets you pick it **per provider**:

```toml
# ~/.codex/config.toml
model_provider = "openrouter"
model = "anthropic/claude-sonnet-4.5"

[model_providers.openrouter]
name = "OpenRouter"
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"
wire_api = "chat"   # use /v1/chat/completions instead of /v1/responses
```

Point `base_url` at any OpenAI-compatible endpoint. The Chat Completions path is a full peer of the Responses path: streaming (SSE), tool/function calls (including MCP tools over chat completions), `reasoning_effort` (with `Ultra`→`Max` normalization), and `parallel_tool_calls`.

> [!NOTE]
> `wire_api = "chat"` cannot be combined with `supports_websockets` — Chat Completions is HTTP/SSE only.

### 2. Better model metadata via models.dev

When a model slug isn't in the bundled catalog or the provider's `/models` endpoint, upstream Codex falls back to a conservative hardcoded profile (e.g. a 272k context window) and emits a `model metadata not found` warning on every turn — which degrades context-window accounting for well-known third-party models.

`recodex` transparently enriches unknown slugs from the public **[models.dev](https://models.dev)** catalog: it fetches the real **display name** and **context window**, then **caches them on disk** so the lookup never repeats (negative results are remembered for 24h). It's best-effort and fail-safe — on any miss or network error you get the original fallback unchanged, so it can never leave you worse off than upstream. Net effect: plug in any provider and model slug, and the model picker, context accounting, and warnings just work.

<details>
<summary><b>How it works</b></summary>

- The model manager queries `https://models.dev/models.json` (10s timeout) only for slugs missing from the bundled + provider catalogs.
- Results are persisted to `models_dev_cache.json` under your Codex home directory.
- The enrichment is read-only metadata (display name, context window); your configured slug and provider are never changed.

</details>

---

## Quick start

Run `recodex` to start the interactive session, or pass a prompt directly:

```shell
recodex
recodex "explain this codebase to me"
```

Then sign in, or — the BYOK path this fork is built for — point it at your own provider in `~/.codex/config.toml` as shown above.

---

## Docs

- **[Upstream Codex documentation](https://developers.openai.com/codex)** — most of it applies to `recodex` too.
- [Contributing](./docs/contributing.md)
- [Installing & building](./docs/install.md)

---

`recodex` is licensed under the [Apache-2.0 License](LICENSE), same as upstream Codex.
