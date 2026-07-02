<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

> [!IMPORTANT]
> **This is `recodex` — a community fork of [openai/codex](https://github.com/openai/codex).**
> It tracks upstream closely (the entire Codex CLI below works as you'd expect) and adds two things that make **bring-your-own-key (BYOK)** usage first-class: a restored **Chat Completions** wire API and automatic, richer model metadata via **[models.dev](https://models.dev)**.

## What this fork changes

### 1. Chat Completions is back — BYOK, natively

Upstream Codex deprecated the OpenAI **Chat Completions** wire protocol and standardized on the **Responses** API (`/v1/responses`). Most third-party providers and self-hosted servers only speak `/v1/chat/completions`, so this fork restores the full Chat Completions path and lets you pick it **per provider**:

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

Point `base_url` at any OpenAI-compatible endpoint — OpenRouter, ZAI, Together, Groq, Fireworks, or local servers like Ollama, LM Studio, and vLLM. The Chat Completions path is a full peer of the Responses path: streaming (SSE), tool/function calls (including MCP tools over chat completions), `reasoning_effort` (with `Ultra`→`Max` normalization), and `parallel_tool_calls`.

> [!NOTE]
> `wire_api = "chat"` cannot be combined with `supports_websockets` — Chat Completions is HTTP/SSE only.

### 2. Better model metadata via models.dev

When a model slug isn't in the bundled catalog or the provider's `/models` endpoint, upstream Codex falls back to a conservative hardcoded profile (e.g. a 272k context window) and emits a `model metadata not found` warning on every turn — which degrades context-window accounting for well-known third-party models.

This fork transparently enriches unknown slugs from the public **[models.dev](https://models.dev)** catalog: it fetches the real **display name** and **context window**, then **caches them on disk** so the lookup never repeats (negative results are remembered for 24h). It's best-effort and fail-safe — on any miss or network error you get the original fallback unchanged, so it can never leave you worse off than upstream. Net effect: plug in any provider and model slug, and the model picker, context accounting, and warnings just work.

<details>
<summary><b>How it works</b></summary>

- The model manager queries `https://models.dev/models.json` (10s timeout) only for slugs missing from the bundled + provider catalogs.
- Results are persisted to `models_dev_cache.json` under your Codex home directory.
- The enrichment is read-only metadata (display name, context window); your configured slug and provider are never changed.

</details>

---

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
