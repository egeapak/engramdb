# Homebrew formula for EngramDB.
#
# Builds from source and links against Homebrew's own ONNX Runtime rather than
# downloading a prebuilt one. `depends_on "onnxruntime"` is the whole point:
# EngramDB deliberately does not vendor a runtime, because the prebuilt that
# `ort`'s `download-binaries` feature fetches executes quantized models
# incorrectly on AVX-512/AMX hosts — the same text embeds to unrelated vectors
# under CPU load. Homebrew's build is unaffected (verified: identical embeddings,
# cosine 1.000000, under 16 concurrent load threads).
#
# `system-onnxruntime` is used rather than the default `load-dynamic` because a
# formula builds from source with its dependencies already installed, so
# resolving the library at build time via pkg-config is strictly better than
# deferring it to run time — a missing or mismatched runtime becomes a build
# error instead of a degraded install.
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
  depends_on "pkgconf" => :build
  depends_on "rust" => :build

  # The runtime EngramDB will link against. Any version providing C API 24
  # (ONNX Runtime >= 1.24) works; Homebrew currently ships 1.28.
  depends_on "onnxruntime"

  def install
    # `--no-default-features` is required: `load-dynamic` sits in the default
    # feature set, and a linking strategy has to be chosen explicitly when
    # replacing it. `onnxruntime` and `ollama` are the other two defaults.
    system "cargo", "install", *std_cargo_args(path: "crates/engram-cli"),
           "--no-default-features",
           "--features", "onnxruntime,ollama,system-onnxruntime"

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
