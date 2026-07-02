# recodex (npm)

Thin npm launcher for the `recodex` CLI. On first run it downloads the
prebuilt binary for your platform from the
[GitHub release](https://github.com/Kartvya69/recodex/releases) and caches it
under `~/.recodex/bin/`; later runs launch the cached binary directly.

## Install

```shell
npm install -g recodex
recodex --version
```

v0.1.0 ships a Linux x86_64 (glibc) binary. macOS / Windows / arm64 builds
will follow; until then the launcher falls back to the
[Releases page](https://github.com/Kartvya69/recodex/releases) for other
platforms.
