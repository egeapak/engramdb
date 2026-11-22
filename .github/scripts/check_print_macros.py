#!/usr/bin/env python3
"""Fail when product code writes to stdout/stderr around the output formatter.

Run from the repository root:

    python3 .github/scripts/check_print_macros.py              # the CI gate
    python3 .github/scripts/check_print_macros.py --self-test  # test the scanner

Two scans.

1. `crates/engram-cli/src`: no print macro and no direct write to the process
   streams anywhere. `OutputFormatter` writes through an injectable `Sink`
   (real streams in production, an in-memory buffer under `#[cfg(test)]`),
   which is what makes the tier-1 renderer snapshots possible: a snapshot
   needs the rendered bytes as a value, and a bare `println!` sends them where
   no in-process test can read them. Use `outln!` / `errln!` / `outraw!` /
   `errraw!` from `crate::output` (docs/contributors/testing.md).

   Allowed, by exact shape and nothing wider:
     - `commands/hook/protocol.rs`, the whole file: it emits the hook protocol
       document Claude Code parses off stdout, plus the hooks' stderr
       diagnostics, and holds no formatter. The rest of `commands/hook/` is
       scanned like any other file.
     - the four `Sink::Stdout => println!("{}", args),` arms in `output.rs`,
       which are the sink.
     - `clap_complete::generate(.., &mut io::stdout())` in
       `commands/completions.rs`, which hands the stream to clap.
   `output.rs` is NOT exempt as a file.

2. The library crates (`src/` and `crates/engram-{mcp,storage,models,types,
   onnx,dashboard}/src`): nothing may write to **stdout**. The MCP server
   speaks JSON-RPC on stdout and the hooks print a protocol document there, so
   one stray `println!` in a library corrupts both. `eprintln!` is allowed in
   the libraries (there are warning sites; moving them to `tracing` is a
   separate question).

What is skipped:
  - comments and the contents of string and char literals (so a doc comment or
    an error message that names `println!` is not a call);
  - the body of every `#[cfg(test)] mod <name> { ... }`, brace-balanced, and
    the file or directory behind every `#[cfg(test)] mod <name>;`. A print in
    a test is debugging output, not product output. Code *after* a test module
    is scanned: the scanner used to stop at the first `mod tests {`, so
    anything below one was never looked at.
"""

from __future__ import annotations

import pathlib
import re
import sys

CLI_ROOT = "crates/engram-cli/src"
CLI_EXEMPT_FILES = {"crates/engram-cli/src/commands/hook/protocol.rs"}
LIB_ROOTS = [
    "src",
    "crates/engram-mcp/src",
    "crates/engram-storage/src",
    "crates/engram-models/src",
    "crates/engram-types/src",
    "crates/engram-onnx/src",
    "crates/engram-dashboard/src",
]

# `println!(`, `std::println!(`, `::std::eprint! (` ... The look-behind keeps
# `outln!` and `my_println!` out; an optional `std::` path is matched so the
# qualified form (which the old pattern's `(?<![\w:])` let through) is caught.
PRINT_ANY = re.compile(r"(?<![\w])(?:(?:::)?std::)?(eprintln|println|eprint|print)\s*!\s*[(\[{]")
PRINT_STDOUT = re.compile(r"(?<![\w])(?:(?:::)?std::)?(println|print)\s*!\s*[(\[{]")
# `writeln!(io::stdout(), ..)`, `write!(std::io::stderr().lock(), ..)`, ...
WRITE_MACRO_ANY = re.compile(
    r"(?<![\w])(writeln|write)\s*!\s*[(\[{]\s*(?:&mut\s+)?(?:(?:::)?std::)?(?:io::)?(stdout|stderr)\s*\(\s*\)"
)
WRITE_MACRO_STDOUT = re.compile(
    r"(?<![\w])(writeln|write)\s*!\s*[(\[{]\s*(?:&mut\s+)?(?:(?:::)?std::)?(?:io::)?(stdout)\s*\(\s*\)"
)
# `io::stdout().write_all(..)`, `stdout().lock()`, `std::io::stderr().write(..)`
STREAM_METHOD_ANY = re.compile(
    r"(?<![\w])(stdout|stderr)\s*\(\s*\)\s*\.\s*(write_all|write_fmt|write|lock)\b"
)
STREAM_METHOD_STDOUT = re.compile(
    r"(?<![\w])(stdout)\s*\(\s*\)\s*\.\s*(write_all|write_fmt|write|lock)\b"
)

# `Sink::Stdout => println!("{}", args),` and its three siblings.
SINK_ARM = re.compile(
    r'^\s*Sink::(Stdout|Stderr) => (eprintln|println|eprint|print)!\("\{\}", args\),$'
)

CFG_TEST_MOD = re.compile(
    r"#\[cfg\((?P<cfg>[^\]]*)\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>\w+)\s*(?P<open>[{;])"
)


def mask(src: str) -> str:
    """Replace comments and the insides of string/char literals with spaces.

    Newlines are kept, so offsets and line numbers still match the source.
    Enough of Rust's lexer for this job: nested block comments, raw strings
    with any number of `#`, byte strings, and telling a char literal from a
    lifetime.
    """
    out = list(src)
    i, n = 0, len(src)

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        two = src[i : i + 2]
        if two == "//":
            j = src.find("\n", i)
            j = n if j == -1 else j
            blank(i, j)
            i = j
        elif two == "/*":
            depth, j = 1, i + 2
            while j < n and depth:
                if src[j : j + 2] == "/*":
                    depth, j = depth + 1, j + 2
                elif src[j : j + 2] == "*/":
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == '"' or (
            c in "rb" and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_"))
            and re.match(r'(?:br|rb|r|b)#*"', src[i : i + 40])
            and re.match(r'(?:br|rb|r)#*"|b"', src[i : i + 40])
        ):
            m = re.match(r'(br|rb|r|b)?(#*)"', src[i : i + 40])
            prefix, hashes = m.group(1) or "", m.group(2)
            start = i + m.end()
            if "r" in prefix:
                end_tok = '"' + hashes
                j = src.find(end_tok, start)
                j = n if j == -1 else j
                blank(start, j)
                i = min(n, j + len(end_tok))
            else:
                j = start
                while j < n and src[j] != '"':
                    j += 2 if src[j] == "\\" else 1
                blank(start, min(j, n))
                i = min(n, j + 1)
        elif c == "'":
            m = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'])'", src[i : i + 14])
            if m:
                blank(i + 1, i + m.end() - 1)
                i += m.end()
            else:
                i += 1  # a lifetime or a label
        else:
            i += 1
    return "".join(out)


def cfg_is_test(cfg: str) -> bool:
    """`test`, `all(test, unix)`, `any(test, feature = "x")`; not `not(test)`."""
    if re.search(r"\bnot\s*\(\s*test\b", cfg):
        return False
    return re.search(r"(?<![\w=\"])test(?![\w\"])", cfg) is not None


PATH_ATTR = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')


def test_regions(masked: str, src: str) -> tuple[list[tuple[int, int]], list[str]]:
    """Byte ranges of inline `#[cfg(test)] mod x { .. }` bodies, and the
    out-of-line `#[cfg(test)] mod x;` modules: the module name, or
    `path:<file>` when a `#[path = "<file>"]` attribute names the file."""
    regions: list[tuple[int, int]] = []
    files: list[str] = []
    for m in CFG_TEST_MOD.finditer(masked):
        if not cfg_is_test(m.group("cfg")):
            continue
        if m.group("open") == ";":
            # The attribute's string was blanked by `mask`; read the source.
            named = PATH_ATTR.search(src[m.start() : m.end()])
            files.append(f"path:{named.group(1)}" if named else m.group("name"))
            continue
        depth, j = 1, m.end()
        while j < len(masked) and depth:
            if masked[j] == "{":
                depth += 1
            elif masked[j] == "}":
                depth -= 1
            j += 1
        regions.append((m.start(), j))
    return regions, files


def scan_text(path: str, src: str, patterns: list[re.Pattern], allow) -> tuple[list[str], list[str]]:
    """Offending lines in one file, and the test-only module names it declares."""
    masked = mask(src)
    regions, test_mods = test_regions(masked, src)
    lines = src.split("\n")
    hits: dict[int, str] = {}
    for pat in patterns:
        for m in pat.finditer(masked):
            if any(a <= m.start() < b for a, b in regions):
                continue
            line_no = masked.count("\n", 0, m.start())
            line = lines[line_no]
            if allow(path, line):
                continue
            hits.setdefault(line_no, f"{path}:{line_no + 1}: {line.strip()}")
    return [hits[k] for k in sorted(hits)], test_mods


def scan_tree(root: str, patterns: list[re.Pattern], allow, exempt_files=frozenset()) -> list[str]:
    base = pathlib.Path(root)
    if not base.is_dir():
        # A moved crate must not turn the gate into a silent pass.
        print(f"::error::{root} does not exist; update check_print_macros.py")
        sys.exit(1)
    files = sorted(p for p in base.rglob("*.rs"))
    results: dict[pathlib.Path, list[str]] = {}
    skipped: set[pathlib.Path] = set()
    for path in files:
        src = path.read_text(encoding="utf-8")
        offenders, test_mods = scan_text(path.as_posix(), src, patterns, allow)
        results[path] = offenders
        # `#[cfg(test)] mod name;` in `a/b.rs` is `a/b/name.rs` or
        # `a/b/name/`; in `a/mod.rs`, `a/lib.rs` or `a/main.rs` it is
        # `a/name.rs` or `a/name/`.
        owner = path.parent if path.name in ("mod.rs", "lib.rs", "main.rs") else path.with_suffix("")
        for name in test_mods:
            if name.startswith("path:"):
                # `#[path]` is relative to the declaring file's directory.
                skipped.add(path.parent / name[len("path:") :])
            else:
                skipped.add(owner / f"{name}.rs")
                skipped.add(owner / name)
    offenders: list[str] = []
    for path in files:
        if path.as_posix() in exempt_files:
            continue
        if any(s == path or s in path.parents for s in skipped):
            continue
        offenders.extend(results[path])
    return offenders


def allow_cli(path: str, line: str) -> bool:
    if path.endswith("/output.rs") and SINK_ARM.match(line):
        return True
    if path.endswith("/commands/completions.rs") and re.search(
        r"\bgenerate\s*\(.*&mut\s+(?:std::)?io::stdout\(\)\s*\)", line
    ):
        return True
    return False


def allow_none(_path: str, _line: str) -> bool:
    return False


CLI_PATTERNS = [PRINT_ANY, WRITE_MACRO_ANY, STREAM_METHOD_ANY]
LIB_PATTERNS = [PRINT_STDOUT, WRITE_MACRO_STDOUT, STREAM_METHOD_STDOUT]


def self_test() -> int:
    """Each fixture names what must and must not be reported."""
    failures = []

    def check(name: str, src: str, patterns, allow, want: list[int], path="crates/engram-cli/src/x.rs"):
        got = [int(o.split(":")[1]) for o in scan_text(path, src, patterns, allow)[0]]
        if got != want:
            failures.append(f"{name}: reported lines {got}, expected {want}")

    # The three holes this script closes.
    check("a std::-qualified macro", 'fn f() {\n    std::println!("x");\n}\n', CLI_PATTERNS, allow_cli, [2])
    check(
        "a print after a test module",
        '#[cfg(test)]\nmod tests {\n    fn t() { println!("in test {{"); }\n}\n\nfn after() {\n    println!("x");\n}\n',
        CLI_PATTERNS, allow_cli, [7],
    )
    check(
        "writeln! to io::stdout()",
        'fn f() {\n    writeln!(io::stdout(), "x").ok();\n    let _ = write!(std::io::stderr(), "y");\n}\n',
        CLI_PATTERNS, allow_cli, [2, 3],
    )
    check(
        "direct stream writes",
        'fn f() {\n    io::stdout().write_all(b"x").ok();\n    let mut o = std::io::stdout().lock();\n}\n',
        CLI_PATTERNS, allow_cli, [2, 3],
    )
    # Every plain form.
    check(
        "the four macros",
        'fn f() {\n    println!("a");\n    eprintln!("b");\n    print!("c");\n    eprint!("d");\n}\n',
        CLI_PATTERNS, allow_cli, [2, 3, 4, 5],
    )
    # What must stay quiet.
    check(
        "comments, strings and our own macros",
        '// println!("x")\n/* eprintln!("y") /* nested */ print!("z") */\nfn f() {\n'
        '    outln!(self, "use println!(\\"x\\") never");\n    let s = r#"println!("raw")"#;\n'
        "    let c = '\"'; let d = '{';\n    errln!(self, \"ok\");\n    my_println!(\"x\");\n}\n",
        CLI_PATTERNS, allow_cli, [],
    )
    check(
        "is_terminal() is not a write",
        "fn f() -> bool {\n    std::io::stdout().is_terminal()\n}\n",
        CLI_PATTERNS, allow_cli, [],
    )
    check(
        "a brace in a string does not end a test module early",
        '#[cfg(test)]\nmod tests {\n    const S: &str = "}";\n    fn t() { println!("x"); }\n}\n',
        CLI_PATTERNS, allow_cli, [],
    )
    check(
        "a lifetime is not a char literal",
        "#[cfg(test)]\nmod tests {\n    fn t<'a>(x: &'a str) -> &'a str { println!(\"x\"); x }\n}\nfn g() { println!(\"y\"); }\n",
        CLI_PATTERNS, allow_cli, [5],
    )
    check(
        "cfg(all(test, unix)) is a test module; cfg(not(test)) is not",
        '#[cfg(all(test, unix))]\nmod unix_tests {\n    fn t() { println!("x"); }\n}\n'
        '#[cfg(not(test))]\nmod real {\n    fn f() { println!("y"); }\n}\n',
        CLI_PATTERNS, allow_cli, [7],
    )
    check(
        "#[cfg(test)] on a fn is not a module: its neighbours are scanned",
        '#[cfg(test)]\nfn helper() {}\n\nfn f() {\n    println!("x");\n}\n',
        CLI_PATTERNS, allow_cli, [5],
    )
    # The allow-list is by exact shape and by file.
    sink = 'impl Sink {\n    fn w(&self) {\n        match self {\n            Sink::Stdout => println!("{}", args),\n            Sink::Stderr => eprintln!("{}", args),\n        }\n        println!("{}", args);\n    }\n}\n'
    check("the Sink arms, in output.rs only", sink, CLI_PATTERNS, allow_cli, [7], path="crates/engram-cli/src/output.rs")
    check("the Sink arms anywhere else", sink, CLI_PATTERNS, allow_cli, [4, 5, 7])
    # The library scan: stdout only.
    check(
        "library crates: stdout is reported, stderr is not",
        'fn f() {\n    eprintln!("warn");\n    println!("x");\n    print!("y");\n    writeln!(std::io::stdout(), "z").ok();\n    writeln!(io::stderr(), "w").ok();\n}\n',
        LIB_PATTERNS, allow_none, [3, 4, 5], path="src/x.rs",
    )
    # Out-of-line test modules are reported to the tree walk.
    _, mods = scan_text(
        "src/lib.rs",
        '#[cfg(test)]\nmod unit;\nmod real;\n#[cfg(test)]\n#[path = "server_tests.rs"]\nmod tests;\n',
        LIB_PATTERNS, allow_none,
    )
    if mods != ["unit", "path:server_tests.rs"]:
        failures.append(f"out-of-line test modules: got {mods}, expected ['unit', 'path:server_tests.rs']")

    for f in failures:
        print(f"self-test FAILED: {f}")
    if not failures:
        print("self-test OK")
    return 1 if failures else 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()

    cli = scan_tree(CLI_ROOT, CLI_PATTERNS, allow_cli, frozenset(CLI_EXEMPT_FILES))
    lib: list[str] = []
    for root in LIB_ROOTS:
        lib.extend(scan_tree(root, LIB_PATTERNS, allow_none))

    if cli:
        for entry in cli:
            print(entry)
        print(
            "::error::engram-cli writes to the process streams outside OutputFormatter's "
            "Sink, which is invisible to the tier-1 snapshots. Use outln! / errln! / "
            "outraw! / errraw! from crate::output (see docs/contributors/testing.md)."
        )
    if lib:
        for entry in lib:
            print(entry)
        print(
            "::error::a library crate writes to stdout. The MCP server speaks JSON-RPC on "
            "stdout and the hooks print a protocol document there; return the text to the "
            "caller, or log through `tracing` (stderr)."
        )
    if cli or lib:
        return 1
    print("OK: engram-cli output goes through OutputFormatter, and no library crate writes to stdout")
    return 0


if __name__ == "__main__":
    sys.exit(main())
