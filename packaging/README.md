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
| Homebrew (`homebrew/engramdb.rb`) | `load-dynamic` (default) | `depends_on "onnxruntime"`, loaded at run time |
| Scoop (`scoop/*.json`) | `load-dynamic` (default) | `"depends": "onnxruntime"`, loaded at run time from `PATH` |
| GitHub release archives | `load-dynamic` (default) | Whatever the user installed — archives hold the binary only |
| `cargo install` | `load-dynamic` (default) | Whatever the user has installed |

No channel ships a runtime, in any form. Three reasons:

1. **Correctness.** The prebuilt ONNX Runtime that `ort`'s `download-binaries`
   feature fetches executes quantized models incorrectly on AVX-512/AMX hosts —
   under CPU load the same text embeds to unrelated vectors, and those vectors
   get persisted. Microsoft's and Homebrew's builds of the same version are
   bit-reproducible. See `docs/contributors/embedding-model-alternatives.md`
   (R6/R9).
2. **One runtime per machine.** A copy we ship cannot be patched by whoever
   maintains the rest of the machine, and a copy sitting beside the binary
   silently wins over the package manager's — the search checks the
   executable's own directory first.
3. **No partial coverage.** Shipping it for some targets and not others is
   worse than not shipping it at all. Intel Mac could never have had one (no
   official x86_64 macOS build past 1.23.x, and Homebrew's is not
   redistributable standalone — it links abseil / onnx / protobuf / re2), so
   bundling elsewhere would mean one platform quietly behaving differently.

A missing runtime is never fatal: `engram_onnx::runtime` probes and validates
the library before `ort` touches it, so EngramDB degrades to keyword search and
`engramdb doctor` says why. That probe is load-bearing — `ort`'s own loader
panics on a missing dylib, and the release profile sets `panic = "abort"`.

### Why nothing links the runtime at build time

Linking against a package manager's ONNX Runtime at build time (via
`ort/pkg-config`) looks like the natural choice where the dependency is
declared and therefore guaranteed present. EngramDB does not offer it, because
of what it records in the executable.

Build-time linking makes `libonnxruntime` a **load-time** dependency. The
dynamic loader resolves it before `main()` runs, so if the library later goes
away — `brew uninstall onnxruntime`, a major bump that changes the dylib's
install name, a relocated Cellar — the binary does not start at all:

```
$ engramdb --version
engramdb: error while loading shared libraries: libonnxruntime.so.1: cannot open
shared object file: No such file or directory
```

No `--version`, and no `doctor` to explain it. The pre-flight probe cannot help,
because no EngramDB code has run yet.

Under `load-dynamic` nothing is recorded, the binary always starts, and the same
situation produces a working CLI that reports the problem and falls back to
keyword search. `depends_on` / `"depends"` still guarantee the runtime is
installed; the strategy only decides how gracefully things fail once something
disturbs it.

A distro package (deb/rpm) that wants an ELF-level, package-manager-verifiable
dependency can still get one by declaring the runtime as a package dependency;
it just will not be recorded in the binary. That trade is deliberate — the
recorded dependency is precisely what removes the ability to report the problem.

A CI job enforces this: the default binary must show no ONNX Runtime entry in
`ldd` and must start with no runtime installed.

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
