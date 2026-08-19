#!/usr/bin/env bash
# The aarch64 half of the comparison, without an aarch64 machine.
#
# `dot_unit`'s NEON backend was deleted by the fearless_simd migration and the
# CI fleet is x86-64, so the obvious question — did Apple Silicon regress? —
# had no obvious way to answer. Two things can still be established off-host,
# and this script does both:
#
#   1. CORRECTNESS, exactly. QEMU user-mode is an architectural emulator: the
#      arithmetic it performs is the arithmetic the hardware performs. The
#      shipped kernel and the deleted NEON backend are both run against a
#      scalar reference over 23 lengths x 8 seeds.
#
#   2. THROUGHPUT, as a static estimate. `llvm-mca` models a named core's
#      pipeline — issue width, port assignment, latencies — and reports cycles
#      for a loop body. Apple M1 is one of the modelled cores.
#
# What this is NOT: a wall-clock benchmark. QEMU timings are meaningless
# (it translates to x86 and runs at whatever speed that takes), and llvm-mca
# assumes a perfect front end with no cache misses or branch mispredicts and
# models steady state only. Treat the cycles/float RATIO as the result and
# ignore the absolutes. A real measurement on real silicon still supersedes
# this the moment anyone has a Mac to hand.
#
#   apt-get install -y qemu-user-static gcc-aarch64-linux-gnu llvm
#   rustup target add aarch64-unknown-linux-gnu
#   ./aarch64.sh
set -euo pipefail
cd "$(dirname "$0")"

TARGET=aarch64-unknown-linux-gnu
CPUS=${CPUS:-"apple-m1 neoverse-n1 cortex-a72"}

need() { command -v "$1" >/dev/null || { echo "error: missing $1 — $2" >&2; exit 1; }; }
need qemu-aarch64-static "apt-get install -y qemu-user-static"
need aarch64-linux-gnu-gcc "apt-get install -y gcc-aarch64-linux-gnu"
need llvm-mca "apt-get install -y llvm"
rustup target list --installed | grep -qx "$TARGET" \
  || { echo "error: rustup target add $TARGET" >&2; exit 1; }

# RUSTFLAGS must be *set* (even empty) rather than left to `.cargo/config.toml`:
# the repo root's config adds `-C link-arg=-fuse-ld=lld` for this target, which
# a cross-link through aarch64-linux-gnu-gcc cannot satisfy ("cannot find ld").
# Cargo joins rustflags arrays across config files, so a local override cannot
# remove it; the env var replaces the config entirely, which is what we want.
export RUSTFLAGS=""

echo "== correctness (QEMU user-mode, architecturally exact) =="
cargo build --release --target "$TARGET" --bin neon_ref >/dev/null
qemu-aarch64-static -L /usr/aarch64-linux-gnu \
  "target/$TARGET/release/neon_ref"

echo
echo "== throughput (llvm-mca static model) =="
cargo build --profile syms --target "$TARGET" --bin neon_ref >/dev/null
BIN="target/$TARGET/syms/neon_ref"
OBJDUMP=$(command -v aarch64-linux-gnu-objdump || command -v objdump)
"$OBJDUMP" -d --no-show-raw-insn "$BIN" > /tmp/simd-probe-aarch64.asm

# Pull the hot loop out of one function: for every backward branch, take the
# instructions from its target up to the branch, and keep whichever such loop
# contains the most `fmla`. Picking the *last* backward branch instead finds
# the scalar tail loop, which is the wrong one and has no fmla at all.
# Written against objdump's `addr:\tmnemonic\toperands` layout.
extract_loop() {
  awk -v want="$1" '
    # `strtonum` is a gawk extension and Ubuntu ships mawk, so parse hex here.
    function hex(s,   i, c, v, d) {
      v = 0
      for (i = 1; i <= length(s); i++) {
        c = tolower(substr(s, i, 1))
        d = index("0123456789abcdef", c) - 1
        if (d < 0) return -1
        v = v * 16 + d
      }
      return v
    }
    $0 ~ "^[0-9a-f]+ <.*"want".*>:" { inf = 1; next }
    inf && /^$/ { inf = 0 }
    inf {
      addr = $1; sub(":", "", addr)
      line = $0; sub(/^[^\t]*\t/, "", line)
      order[++n] = addr; ins[addr] = line; pos[addr] = n
      if (line ~ /^b[.a-z]*[ \t]+[0-9a-f]+/) {
        tgt = line; sub(/^[^ \t]+[ \t]+/, "", tgt); sub(/[ \t].*$/, "", tgt)
        if (hex(tgt) >= 0 && hex(tgt) < hex(addr)) {
          nb++; btgt[nb] = tgt; bend[nb] = addr
        }
      }
    }
    END {
      best = 0; bestlo = 0; besthi = 0
      for (j = 1; j <= nb; j++) {
        lo = pos[btgt[j]]; hi = pos[bend[j]]
        if (lo == 0 || hi == 0 || lo > hi) continue
        c = 0
        for (i = lo; i <= hi; i++) if (ins[order[i]] ~ /fmla/) c++
        if (c > best) { best = c; bestlo = lo; besthi = hi }
      }
      if (best == 0) exit 1
      for (i = bestlo; i <= besthi; i++) {
        t = ins[order[i]]
        # llvm-mca assembles what it is given, and objdump decorates branch
        # targets with `<symbol+0x..>` and a `// b.cond` gloss that it cannot
        # parse. Dropping the branch entirely would understate issue pressure,
        # so rewrite the target to `.` instead: mca models the instruction
        # stream, not control flow, so a self-reference assembles and costs the
        # same.
        sub(/[ \t]*\/\/.*$/, "", t)          # trailing // comment
        sub(/[ \t]*<[^>]*>[ \t]*$/, "", t)    # <symbol+0x..> annotation
        if (t ~ /^(b|bl|cb[nz]+|tb[nz]+)[.a-z]*[ \t]/) sub(/[0-9a-f]+$/, ".", t)
        print "  " t
      }
    }
  ' /tmp/simd-probe-aarch64.asm
}

for pair in "dot_neon:8:NEON intrinsics (deleted)" \
            "dot_fearless:16:fearless_simd (ships)  "; do
  fn=${pair%%:*}; rest=${pair#*:}; floats=${rest%%:*}; label=${rest#*:}
  extract_loop "$fn" > "/tmp/simd-probe-$fn.s" || {
    echo "error: could not locate a hot loop in $fn" >&2; exit 1; }
  fmla=$(grep -c fmla "/tmp/simd-probe-$fn.s" || true)
  [ "$fmla" -gt 0 ] || { echo "error: no fmla in $fn's loop — extraction is wrong" >&2; exit 1; }
  echo "$fn: $(wc -l < /tmp/simd-probe-$fn.s) instructions, $fmla fmla, $floats floats/iter"
done

echo
printf '%-14s %-28s %12s %12s %6s\n' cpu kernel cycles/iter cycles/float IPC
for cpu in $CPUS; do
  for pair in "dot_neon:8:NEON intrinsics (deleted)" \
              "dot_fearless:16:fearless_simd (ships)"; do
    fn=${pair%%:*}; rest=${pair#*:}; floats=${rest%%:*}; label=${rest#*:}
    out=$(llvm-mca -mtriple="$TARGET" -mcpu="$cpu" -iterations=1000 "/tmp/simd-probe-$fn.s")
    cyc=$(echo "$out" | awk '/Total Cycles:/{print $3}')
    ipc=$(echo "$out" | awk '/^IPC:/{print $2}')
    printf '%-14s %-28s %12.3f %12.4f %6s\n' \
      "$cpu" "$label" "$(echo "$cyc/1000" | bc -l)" "$(echo "$cyc/1000/$floats" | bc -l)" "$ipc"
  done
done
echo
echo 'Ratio of the cycles/float column is the result. Absolutes are model'
echo 'output, not measurements — see the header.'
