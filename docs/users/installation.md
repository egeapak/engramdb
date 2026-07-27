# Installation

## Prerequisites

- **Rust 1.75 or later** to build from source. Get it from <https://rustup.rs>.
- **protoc** (Protocol Buffers compiler) — required by LanceDB's build.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
- **ONNX Runtime 1.24 or later** — EngramDB loads it at run time instead of
  embedding it. The prebuilt release archives and the Homebrew / Scoop packages
  supply it for you; only source builds need it installed separately. See
  [ONNX Runtime](#onnx-runtime) below.
- Outbound network access on first run to download the embedding model (~90 MB). After that, engramdb is fully offline.

### ONNX Runtime

EngramDB does not contain a copy of ONNX Runtime, and does not ship one:
release archives hold the binary and nothing else. It resolves a runtime from
your system at startup, in this order:

1. `ORT_DYLIB_PATH`, if set — an explicit path to the library.
2. The directory holding the `engramdb` binary — drop a `libonnxruntime`
   there and it wins over everything below it.
3. Standard package-manager locations: `/opt/homebrew/lib` and `/usr/local/lib`
   on macOS, `/usr/local/lib`, `/usr/lib`, `/usr/lib64` and the multiarch
   directories on Linux.
4. The platform loader's own search path (`PATH` on Windows,
   `LD_LIBRARY_PATH`/`ld.so.conf` on Linux).

Installing it, if you need to:

```bash
brew install onnxruntime          # macOS / Linuxbrew
scoop install onnxruntime         # Windows (manifest ships with EngramDB)
sudo apt-get install -y libonnxruntime   # where packaged
```

This is deliberate. The prebuilt runtime that would otherwise be compiled in
executes quantized models incorrectly on some AVX-512/AMX CPUs — under load the
same text embeds to an unrelated vector, and those vectors are persisted.
Packaged builds do not have the defect, and a runtime installed separately can
be patched without rebuilding EngramDB.

**A missing runtime is not fatal.** EngramDB reports it and falls back to
keyword search rather than failing:

```
engramdb doctor      # the "ONNX Runtime" check names the library and version,
                     # or explains what to install
```

### Platform support

The default build uses **ONNX Runtime**, fetched as a prebuilt binary for **Linux (x86_64/aarch64)**, **Windows (x86_64/aarch64)**, and **Apple Silicon macOS (aarch64)** — the platforms with official release binaries.

**Intel Mac (`x86_64-apple-darwin`)** works like every other platform now, with one wrinkle: Microsoft dropped x86_64 macOS builds after 1.23.x, so the release archive for this target is the only one that does **not** carry a copy of the runtime. Install it yourself:

```bash
brew install onnxruntime     # has an Intel bottle
```

Until you do, `engramdb` still starts and runs — `doctor` reports the missing runtime and search falls back to keyword matching.

EngramDB previously shipped a pure-Rust `tract` backend for this target, because the build had to link a runtime at build time and none existed. Loading the runtime at startup removed that constraint, so Intel Mac now uses the same ONNX path as everywhere else — the int8 model rather than tract's fp32, roughly 3× faster, with NLI and T5 titling available. A store built on the old tract backend records a different model fingerprint and will prompt a one-time `engramdb reindex --embeddings-only`.

## Install

### From the GitHub repository

```bash
cargo install --git https://github.com/egeapak/engramdb
```

This builds with default features (`ollama` enabled). The binary lands in `~/.cargo/bin/engramdb`.

### With a package manager

Both packages depend on ONNX Runtime, so there is nothing else to install:

```bash
brew install egeapak/engramdb/engramdb     # macOS / Linuxbrew
scoop install engramdb                      # Windows
```

### Build from a local checkout

```bash
git clone https://github.com/egeapak/engramdb
cd engramdb
cargo build --release
# binary at target/release/engramdb
```

To install your local build onto your `PATH`:

```bash
cargo install --path .
```

Choosing a different ONNX Runtime strategy means replacing the default one, so
`--no-default-features` is required and the other defaults are named explicitly:

```bash
# Statically link a downloaded runtime (the pre-0.9 behavior; see the caveat
# in the feature table).
cargo build --release --no-default-features \
    --features onnxruntime,ollama,bundled-onnxruntime
```

### Feature flags

| Flag | Default | What it does |
|------|---------|--------------|
| `load-dynamic` | **on** | Load ONNX Runtime at run time (`dlopen`). Nothing is required at build time, and the binary works against any installed runtime >= 1.24. |
| `bundled-onnxruntime` | off | Download and statically link a prebuilt runtime — the historical default. **Not recommended**: that prebuilt mis-executes quantized models on AVX-512/AMX hosts. Kept for platforms with no packaged runtime and for hermetic builds. Needs `--no-default-features`. |
| `ollama` | on | Adds the Ollama embedding backend (uses `reqwest`). Turn off for a pure-ONNX, offline-only build with no extra deps: `cargo install --git ... --no-default-features`. |
| `coreml` | off | Apple Core ML execution provider for ONNX models (Neural Engine / GPU). macOS only. |
| `xnnpack` | off | XNNPACK CPU execution provider for ONNX. Useful for A/B benchmarking. |

## Verify

```bash
engramdb --version
engramdb doctor
```

`doctor` reports the embedding backend, model cache path, daemon reachability, and platform paths. Missing-model warnings before your first store are normal — models download on first use.

## What gets installed where

EngramDB writes to platform-standard locations via the `dirs` crate. Each respects an environment-variable override:

| Purpose | macOS | Linux | Env override |
|---------|-------|-------|--------------|
| Models (embeddings, NLI, reranker) | `~/Library/Caches/engramdb/models/` | `~/.cache/engramdb/models/` | — |
| Global config | `~/Library/Application Support/engramdb/` | `~/.config/engramdb/` | `ENGRAMDB_CONFIG_DIR` |
| Global data + project registry | `~/Library/Application Support/engramdb/` | `~/.local/share/engramdb/` | `ENGRAMDB_DATA_DIR` |
| Daemon endpoint | `$XDG_RUNTIME_DIR/engramdb/daemon.sock` (Linux) or the cache dir (macOS); a named pipe (`\\.\pipe\engramdb-<hash>`) on Windows | same | `ENGRAMDB_DAEMON_SOCKET` |

Per-project state lives in `<project>/.engramdb/`. The vector index and personal-visibility memories live under `<global_data_dir>/projects/<project_id>/`. See [projects-and-worktrees.md](./projects-and-worktrees.md) for the full layout.

## Uninstall

```bash
cargo uninstall engramdb
# Optionally also remove all data:
rm -rf ~/.local/share/engramdb ~/.config/engramdb ~/.cache/engramdb   # Linux
rm -rf "~/Library/Application Support/engramdb" "~/Library/Caches/engramdb"   # macOS
```

Per-project `.engramdb/` directories are not touched — remove them manually.
