# Packaging

Package definitions for distributing EngramDB. They are kept in-tree so the
runtime contract below is versioned next to the code that depends on it, but
they are **not** consumed by any build: publishing means copying them into a
Scoop bucket or a MacPorts repository and filling in the release hashes.

The **Homebrew formula lives in a separate tap repo**, not here. The runtime
contract below still governs it — a tap formula must declare
`depends_on "onnxruntime"` and build with default features, which is what makes
the binary load the runtime at startup instead of recording a load-time
dependency on it. See "Why not link at build time" below before changing how
that formula builds.

## The runtime contract

EngramDB does not contain an ONNX Runtime. Every package must arrange for one,
and the packages differ only in *how*:

| Channel | Strategy | Where the runtime comes from |
|---|---|---|
| Homebrew (separate tap repo) | `load-dynamic` (default) | `depends_on "onnxruntime"`, loaded at run time |
| Scoop (`scoop/*.json`) | `load-dynamic` (default) | `"depends": "onnxruntime"`, loaded at run time from `PATH` |
| MacPorts (`macports/Portfile`) | `load-dynamic` (default) | **Nothing declares it** — MacPorts has no `onnxruntime` port, so the user supplies one. See "MacPorts" below |
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

## MacPorts

`macports/Portfile` builds EngramDB from source on macOS. MacPorts differs from
the other two channels in the two ways that decided everything about it: it
**builds** rather than downloading a binary, and it builds **offline**.

### Offline builds, and the 678-line crate list

The cargo PortGroup passes `--frozen` and replaces crates.io with a local
directory, so cargo may not reach the network during the build. Every crate in
`Cargo.lock` has to be declared in the Portfile as a `name version sha256`
triple; MacPorts fetches each one from `static.crates.io` and verifies it
before the build phase starts. That is 678 entries, and it changes on every
dependency bump.

It is generated, never hand-written:

```
packaging/macports/gen-crates.py --update packaging/macports/Portfile
```

The Portfile marks the region with `# BEGIN cargo.crates` / `# END cargo.crates`
and the script rewrites only that. `--check` exits non-zero when the block has
drifted from `Cargo.lock`; the `macports-crates` CI job runs exactly that, so a
dependency bump that forgets the Portfile fails the build rather than the port.

Two things make this workable and both are worth not breaking:

- **Nothing in the tree is a git dependency.** All 678 are plain crates.io
  crates with checksums, so the Portfile needs no `cargo.crates_github`
  5-tuples — which are hand-maintained and pin a commit. `deny.toml`'s
  `unknown-git = "deny"` is what keeps this true; the generator errors out
  rather than silently dropping a crate if it ever stops being true.
- **A default-feature build needs no network.** Verified rather than assumed:
  under `load-dynamic` the `ort-sys` build script emits only
  `cargo:rustc-check-cfg=cfg(link_error)` — no download, no link directive, no
  ONNX Runtime of any kind at build time. This is the same property that makes
  the release archives portable, and here it is what makes the port legal.

### Build dependencies

- **`protoc`**, via `path:bin/protoc:protobuf`. Six lance crates compile
  `.proto` files in their build scripts. The dependency is `path:`-style, not
  `port:protobuf`, because `protobuf` *conflicts* with `protobuf3-cpp` and
  `protobuf-cpp` while all three install `${prefix}/bin/protoc` — a hard
  `port:` dependency would force a machine that already has one of the others
  to uninstall it.
- **Nothing else.** In particular **no `cmake`**: `aws-lc-sys` (reached through
  `rustls`, which every TLS path in the tree uses) has a CMake builder and a
  pure-`cc` builder, and it picks `cc` on both macOS targets. Check that again
  if `aws-lc-sys` is ever bumped across a major version.

### Tests do not run in the port

`test.run` must stay off. The dev-dependencies enable `ort/download-binaries`,
whose build script downloads a prebuilt runtime from `cdn.pyke.io` — network
during the build, which MacPorts forbids, and a tarball that is not a crate and
so cannot be declared in `cargo.crates`. There is no way to express it. The
release workflow already gates every tag on the full suite, so nothing is lost
by leaving it off here.

### The dependency that does not exist

**MacPorts has no `onnxruntime` port.** The index has `py-onnx` and friends —
the Python bindings for the ONNX *format* — and no C++ runtime at all. So the
port cannot declare the dependency the Homebrew formula and the Scoop manifest
both declare, and `macports/Portfile` deliberately declares nothing.

That is degraded, not broken, and only because of `load-dynamic`: the binary
never links the runtime, so it starts either way, `engramdb doctor` names every
path it searched, and search falls back to keyword matching. `${prefix}/lib`
(`/opt/local/lib`) is one of the searched paths, so dropping a
`libonnxruntime.dylib` there is the whole fix. The port's `notes` says so.

Two non-options, so they do not get re-proposed:

- **`bundled-onnxruntime`** would download and statically link a runtime. It
  cannot be used here twice over: the download is network-at-build-time, and
  the thing it downloads is the pyke prebuilt that mis-executes quantized
  models on AVX-512/AMX hosts (see the top of this file). One of those is a
  MacPorts rule and the other is a correctness rule.
- **Build-time linking** against a MacPorts runtime — see "Why nothing links
  the runtime at build time" above. The reasoning is unchanged by which
  package manager provides the library.

**The fix is a MacPorts `onnxruntime` port**, and the work is mostly ONNX
Runtime's, not ours. MacPorts already has `abseil`, `protobuf`, `re2`,
`flatbuffers`, `eigen3` and `cmake`; it does **not** have `cpuinfo`, `nsync`,
`ms-gsl`, `date` or `safeint`, all of which ONNX Runtime's CMake pulls in with
`FetchContent` at configure time — which is itself network-during-build, so
each one needs either a companion port or an extra checksummed `distfiles`
entry. That is a separate submission with its own review, not a change to this
repo.

Once such a port exists, three lines change here:

1. `depends_lib-append port:onnxruntime` in `macports/Portfile`.
2. Delete the runtime paragraph from the Portfile's `notes`.
3. Move the MacPorts row of the runtime-contract table up to say
   `depends_lib`, like Homebrew's.

### Things that would break the port from inside this repo

- **A git dependency, or a crate from any registry other than crates.io.**
  `gen-crates.py` refuses to generate rather than emitting a list that is
  quietly short.
- **A `[target.<something>-apple-darwin]` block in `.cargo/config.toml`.**
  Today that file only configures the two Linux triples, so it never matches
  during a MacPorts build. Cargo lets a project-level `[target.…] rustflags`
  override `$CARGO_HOME`'s `[build] rustflags`, which is where the PortGroup
  puts `--remap-path-prefix` and the wrapped linker — so a Darwin block there
  would silently drop both.
- **`-C target-cpu=…` anywhere.** MacPorts serves prebuilt archives from its
  own builders, so the binary has to run on CPUs the builder never saw. The
  one SIMD path EngramDB owns (`ops::compress::dot_unit`) dispatches at run
  time on `fearless_simd`'s `Level::new()`; keep it that way.

## Publishing checklist

1. Tag a release so the archives exist.
2. Scoop: copy `scoop/*.json` into the bucket, update `version` and the `hash`
   of every architecture (`scoop hash <url>`).
3. Homebrew: update the formula in the tap repo (`url` + `sha256` for the source
   tarball). Keep `depends_on "onnxruntime"` and the default-feature build.
4. MacPorts: set `github.setup`'s version in `macports/Portfile` to the tag,
   refresh the crate list, and fill in the placeholder source checksums:

   ```
   packaging/macports/gen-crates.py --update packaging/macports/Portfile
   port checksum engramdb        # prints the rmd160 / sha256 / size to paste
   port lint --nitpick engramdb
   ```

5. Verify each one actually resolved a runtime rather than silently falling
   back to keyword search:

   ```
   engramdb doctor          # must report an "ONNX Runtime" check that passes
   ```

   On MacPorts this check is *expected to fail* until the user supplies a
   runtime — there is no `onnxruntime` port to depend on. Confirm instead that
   `doctor` reports the runtime as missing and lists `/opt/local/lib` among the
   paths it searched, and that `engramdb --version` still works.

## Upgrades and the shared daemon

The daemon caches provider bundles for its whole lifetime, so one running
across an upgrade keeps serving the *previous* release's models. That is not
cosmetic: the client fingerprints the embedding model it expects, so
`reindex --embeddings-only` re-embeds via the old daemon, stamps the old model
id, and `doctor` immediately reports a mismatch again — running the suggested
command can never converge. This happened on 0.8.0 → 0.9.0, where the embedding
default moved from all-MiniLM-L6 to L12.

Clients handle this themselves as of protocol 4: `Ping` returns the daemon's
crate version, and a daemon older than the client is asked to shut down so a
current one replaces it. **No package manager post-install step is needed**,
which is what makes it work for `cargo install` and manual downloads too —
Homebrew has no post-install hook for binary-only formulae anyway.

Bump `PROTOCOL_VERSION` when a release changes a model default, even if the
wire format is untouched; it is the explicit lever for evicting stale daemons.
To force it by hand:

```
engramdb daemon restart
```

## Version pinning

`ort` 2.0.0-rc.12 requires ONNX Runtime C API version **24**, so any runtime
from **1.24** onward works. `engram_onnx::runtime::REQUIRED_API_VERSION` is the
single source of truth; a runtime that is too old is rejected with a message
naming its version rather than failing inside `ort`.
