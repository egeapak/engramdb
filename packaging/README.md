# Packaging

Package definitions for distributing EngramDB. They are kept in-tree so the
runtime contract below is versioned next to the code that depends on it, but
they are **not** consumed by any build: publishing means copying them into a
Homebrew tap or a Scoop bucket and filling in the release hashes.

## The runtime contract

EngramDB does not contain an ONNX Runtime. Every package must arrange for one,
and the packages differ only in *how*:

| Channel | Strategy | Where the runtime comes from |
|---|---|---|
| Homebrew (`homebrew/engramdb.rb`) | `system-onnxruntime` | `depends_on "onnxruntime"`, linked at build time via pkg-config |
| Scoop (`scoop/*.json`) | `load-dynamic` (default) | `"depends": "onnxruntime"`, loaded at run time from `PATH` |
| GitHub release archives | `load-dynamic` (default) | Microsoft's official build, shipped beside the binary |
| `cargo install` | `load-dynamic` (default) | Whatever the user has installed |

Two reasons it works this way rather than statically linking one in:

1. **Correctness.** The prebuilt ONNX Runtime that `ort`'s `download-binaries`
   feature fetches executes quantized models incorrectly on AVX-512/AMX hosts —
   under CPU load the same text embeds to unrelated vectors, and those vectors
   get persisted. Microsoft's and Homebrew's builds of the same version are
   bit-reproducible. See `docs/contributors/embedding-model-alternatives.md`
   (R6/R9).
2. **One runtime per machine.** A statically linked copy cannot be patched by
   the package manager that installed everything else.

A missing runtime is never fatal: `engram_onnx::runtime` probes and validates
the library before `ort` touches it, so EngramDB degrades to keyword search and
`engramdb doctor` says why. That probe is load-bearing — `ort`'s own loader
panics on a missing dylib, and the release profile sets `panic = "abort"`.

## Scoop needs an `onnxruntime` manifest, so one ships here

Unlike Homebrew, Scoop has **no** `onnxruntime` package — not in Main, Extras,
or Versions (checked against a working control). `scoop/onnxruntime.json` is
therefore part of this repo's packaging rather than a dependency we can simply
reference. It installs Microsoft's official Windows build and puts its `lib`
directory on `PATH`, which is what lets `engramdb.exe` find `onnxruntime.dll`.

If Scoop's Main bucket ever gains an `onnxruntime` manifest, drop ours and keep
the `"depends"` line as-is.

## Publishing checklist

1. Tag a release so the archives exist.
2. Homebrew: copy `homebrew/engramdb.rb` into the tap, update `url` and
   `sha256` for the source tarball.
3. Scoop: copy `scoop/*.json` into the bucket, update `version` and the `hash`
   of every architecture (`scoop hash <url>`).
4. Verify each one actually resolved a runtime rather than silently falling
   back to keyword search:

   ```
   engramdb doctor          # must report an "ONNX Runtime" check that passes
   ```

## Version pinning

`ort` 2.0.0-rc.12 requires ONNX Runtime C API version **24**, so any runtime
from **1.24** onward works. `engram_onnx::runtime::REQUIRED_API_VERSION` is the
single source of truth; a runtime that is too old is rejected with a message
naming its version rather than failing inside `ort`.
