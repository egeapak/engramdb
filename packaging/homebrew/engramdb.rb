# Homebrew formula for EngramDB.
#
# Builds from source and uses Homebrew's own ONNX Runtime rather than a
# downloaded prebuilt. `depends_on "onnxruntime"` is the whole point:
# EngramDB deliberately does not vendor a runtime, because the prebuilt that
# `ort`'s `download-binaries` feature fetches executes quantized models
# incorrectly on AVX-512/AMX hosts — the same text embeds to unrelated vectors
# under CPU load. Homebrew's build is unaffected (verified: identical embeddings,
# cosine 1.000000, under 16 concurrent load threads).
#
# It builds with the default `load-dynamic` strategy, which resolves the runtime
# at run time even though `depends_on` guarantees it is present at build time.
# That matters when the guarantee stops holding — `brew uninstall onnxruntime`,
# a major-version bump that changes the dylib's install name, a relocated
# Cellar. Linking at build time would record libonnxruntime as a load-time
# dependency, and the dynamic loader resolves those before `main()` runs, so a
# missing library would mean `engramdb` does not start *at all*: no `--version`,
# and no `doctor` to explain why. Resolving at run time records nothing, so the
# binary always starts and degrades to keyword search with an actionable
# message.
#
# To publish: copy this file into a tap (`homebrew-engramdb/Formula/`), fill in
# the release tarball `sha256`, and bump both on each release.
class Engramdb < Formula
  desc "Project-scoped persistent memory store for coding agents"
  homepage "https://github.com/egeapak/engramdb"
  url "https://github.com/egeapak/engramdb/archive/refs/tags/v0.8.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/egeapak/engramdb.git", branch: "master"

  depends_on "protobuf" => :build
  depends_on "rust" => :build

  # The runtime EngramDB will load at startup. Any version providing C API 24
  # (ONNX Runtime >= 1.24) works; Homebrew currently ships 1.28.
  depends_on "onnxruntime"

  def install
    # Default features: `load-dynamic` is already the default strategy, so no
    # feature juggling is needed. See the note above for why the runtime is
    # resolved at run time even with the dependency guaranteed.
    system "cargo", "install", *std_cargo_args(path: "crates/engram-cli")

    generate_completions_from_executable(bin/"engramdb", "completions")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/engramdb --version")

    # `doctor` reports the ONNX Runtime it resolved. Proving the dependency is
    # actually wired up is the one thing worth testing here: a formula that
    # silently produced a keyword-only build would otherwise look healthy.
    output = shell_output("#{bin}/engramdb doctor 2>&1", 0)
    assert_match "ONNX Runtime", output
  end
end
