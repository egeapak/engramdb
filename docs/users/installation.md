# Installation

## Prerequisites

- **A recent stable Rust toolchain** to build from source. Get it from <https://rustup.rs>.
- **protoc** (Protocol Buffers compiler) — required by LanceDB's build.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
- **ONNX Runtime 1.24 or later.** EngramDB loads it when it starts. It is not
  inside the binary and not inside any release archive. The Homebrew package
  installs it as a dependency. In every other case (release archive,
  `cargo install`, local build) you install it yourself. See
  [ONNX Runtime](#onnx-runtime) below.
- Network access on first use, to download the models. The default embedding
  model is about 33 MB. The reranker (about 37 MB) and the T5 title model
  (about 77 MB) are also on by default. After that, EngramDB works offline.

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
sudo apt-get install -y libonnxruntime   # where packaged
```

Windows: download the official ONNX Runtime zip from the Microsoft releases
page, unpack it, and set `ORT_DYLIB_PATH` to `onnxruntime.dll` (or put its
`lib` folder on `PATH`). EngramDB has no published Scoop package yet.

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

The default build links no ONNX Runtime at build time and downloads none. The
same rule holds on every platform: install a runtime 1.24 or later, or point
`ORT_DYLIB_PATH` at one.

Official Microsoft builds exist for Linux (x86_64, aarch64), Windows (x86_64,
aarch64) and Apple Silicon macOS. Microsoft stopped building for Intel Mac
(`x86_64-apple-darwin`) after 1.23, so on that target use Homebrew, which has
an Intel bottle:

```bash
brew install onnxruntime
```

Without a runtime `engramdb` still starts. `engramdb doctor` reports the
missing runtime and search falls back to keyword matching.

Older versions used a pure-Rust `tract` backend on Intel Mac. A store built
with it records a different model fingerprint; run
`engramdb reindex --embeddings-only` once.

## Install

### From the GitHub repository

```bash
cargo install --locked --git https://github.com/egeapak/engramdb engram-cli
```

The package is `engram-cli`; the binary it installs is `engramdb`. The package
name is required, because the repository holds other packages that also have
binaries. This builds with default features (`ollama` enabled). The binary
lands in `~/.cargo/bin/engramdb`.

### With a package manager

The Homebrew package depends on ONNX Runtime, so there is nothing else to
install:

```bash
brew install egeapak/tap/engramdb     # macOS / Linuxbrew
```

There is no published Scoop package yet. On Windows, use a release archive or
`cargo install`, and install ONNX Runtime as described
[above](#onnx-runtime).

### Build from a local checkout

```bash
git clone https://github.com/egeapak/engramdb
cd engramdb
cargo build --release
# binary at target/release/engramdb
```

To install your local build onto your `PATH`:

```bash
cargo install --locked --path crates/engram-cli
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
| `ollama` | on | Adds the Ollama embedding backend (uses `reqwest`). To build without it, name the other default features, because `--no-default-features` alone also turns off ONNX: `cargo install --locked --git https://github.com/egeapak/engramdb engram-cli --no-default-features --features onnxruntime,load-dynamic`. |
| `coreml` | off | Apple Core ML execution provider for ONNX models (Neural Engine / GPU). macOS only. |
| `xnnpack` | off | XNNPACK CPU execution provider for ONNX. Useful for A/B benchmarking. Needs a runtime built with XNNPACK; does not build together with `bundled-onnxruntime`. |

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
| Global config (`registry.json`, `config.toml` with `[daemon]`) | `~/Library/Application Support/engramdb/` | `~/.config/engramdb/` | `ENGRAMDB_CONFIG_DIR` |
| Global data + project registry | `~/Library/Application Support/engramdb/` | `~/.local/share/engramdb/` | `ENGRAMDB_DATA_DIR` |
| Daemon folder (socket, lock, log) | `~/Library/Caches/engramdb/daemon/` | `$XDG_RUNTIME_DIR/engramdb/` (else `~/.cache/engramdb/daemon/`); a named pipe (`\\.\pipe\engramdb-<hash>`) on Windows | `ENGRAMDB_DAEMON_SOCKET` |

Per-project state lives in `<project>/.engramdb/`. The vector index and personal-visibility memories live under `<global_data_dir>/projects/<project_id>/`. See [projects-and-worktrees.md](./projects-and-worktrees.md) for the full layout.

## Uninstall

```bash
cargo uninstall engram-cli
# Optionally also remove all data:
rm -rf ~/.local/share/engramdb ~/.config/engramdb ~/.cache/engramdb   # Linux
rm -rf "~/Library/Application Support/engramdb" "~/Library/Caches/engramdb"   # macOS
```

Per-project `.engramdb/` directories are not touched — remove them manually.
