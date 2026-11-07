//! Embed `web/` into the binary: one table row per file (served path,
//! bytes, MIME type, ETag).
//!
//! Generated rather than hand-written so a new JS module cannot be forgotten
//! in the table — the failure mode would be a 404 on an `import` that works
//! in `--assets-dir` dev mode and breaks only in the shipped binary. The
//! `imports_resolve` test still checks every `import` against the table.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("web");
    // A directory path makes cargo rescan the whole tree for changes.
    println!("cargo:rerun-if-changed=web");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();

    let mut out = String::from("pub(crate) static ASSETS: &[Asset] = &[\n");
    for rel in &files {
        let abs = root.join(rel);
        let bytes = std::fs::read(&abs).unwrap();
        let served = rel.to_string_lossy().replace('\\', "/");
        let mime = mime_for(&served);
        writeln!(
            out,
            "    Asset {{ path: {served:?}, bytes: include_bytes!({abs:?}), mime: {mime:?}, etag: \"\\\"{:016x}\\\"\" }},",
            fnv1a(&bytes),
            abs = abs.to_string_lossy(),
        )
        .unwrap();
    }
    out.push_str("];\n");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("assets.rs"), out).unwrap();

    // The layout-relevant JS (the server's layouts are Rust ports of it):
    // its hash is pinned with `layout::LAYOUT_VERSION` by a test, and salts
    // every layout cache digest. `layout::js_hash` computes the same over
    // the embedded (or `--assets-dir`) files.
    let mut hash = String::from("pub const LAYOUT_JS_FILES: &[&str] = &[\n");
    for f in LAYOUT_JS_FILES {
        writeln!(hash, "    {f:?},").unwrap();
    }
    hash.push_str("];\n");
    let parts: Vec<(&str, Vec<u8>)> = LAYOUT_JS_FILES
        .iter()
        .map(|f| (*f, std::fs::read(root.join(f)).unwrap()))
        .collect();
    writeln!(
        hash,
        "pub const LAYOUT_JS_HASH: &str = {:?};",
        layout_hash(&parts)
    )
    .unwrap();
    std::fs::write(out_dir.join("layout_js.rs"), hash).unwrap();
}

/// Must list what `src/layout/` ports, and agree with `layout::js_hash`.
const LAYOUT_JS_FILES: &[&str] = &[
    "js/graph/input.js",
    "js/graph/labels.js",
    "js/graph/opened.js",
    "js/graph/radius.js",
    "js/graph/scope-force.js",
    "js/graph/scope-overview.js",
    "js/graph/server-layout.js",
];

/// FNV-1a 64 over each file's path, a NUL, its bytes and a NUL, carriage
/// returns left out (a Windows checkout may add them): a change detector,
/// not a security boundary. A copy of `layout::js_hash`.
fn layout_hash(parts: &[(&str, Vec<u8>)]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes.iter().filter(|&&b| b != b'\r') {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for (path, bytes) in parts {
        eat(path.as_bytes());
        eat(&[0]);
        eat(bytes);
        eat(&[0]);
    }
    format!("{h:016x}")
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // Editor droppings and dotfiles are not part of the frontend.
        if name.starts_with('.') || name.ends_with('~') {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, out);
        } else {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

/// Must agree with `assets::mime_for` (the dev-mode path); a test checks it.
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// FNV-1a 64: an ETag only needs to change when the bytes do, and a build
/// script should not pull in a hashing crate for that.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
