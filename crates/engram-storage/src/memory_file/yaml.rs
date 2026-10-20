//! The one place memory-file YAML is read and written.
//!
//! Every frontmatter and hidden-metadata block goes through these two
//! functions, so the parser and emitter options are set once. Memory files
//! arrive with a cloned repository, so the reader is an untrusted-input
//! boundary: `serde-saphyr` is pure Rust, never panics on malformed input, and
//! enforces a size/alias budget against expansion bombs.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::Result;

/// Parse one YAML document.
///
/// `with_snippet` is off: the error's `Display` would otherwise embed a
/// multi-line excerpt of the file, and these errors reach single-line log and
/// CLI output.
pub(super) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T> {
    let options = serde_saphyr::options! {
        with_snippet: false,
    };
    Ok(serde_saphyr::from_str_with_options(input, options)?)
}

/// Serialize one YAML document.
///
/// - `prefer_block_scalars` is off so every string stays on its own line, in
///   quotes when it needs them. A folded (`>`) or literal (`|`) block would
///   spread a value over several lines, and `split_frontmatter` and the hidden
///   `<!-- engramdb -->` block are both found by scanning lines.
/// - `yaml_12` turns off the YAML 1.1 quoting checks, so timestamps and words
///   such as `on`/`no` stay unquoted. That keeps the bytes of a typical file
///   identical to what `serde_yaml_ng` wrote, and with it the file's
///   `content_sha256`. `no_lang_directive` drops the `%YAML 1.2` header that
///   `yaml_12` would otherwise emit.
///
/// Not every file is byte-identical: where a string needs quotes (`''`,
/// `'a: b'`, `'true'`), this writes double quotes and `serde_yaml_ng` wrote
/// single quotes, and a multi-line string is now one escaped line instead of
/// a `|-` block. Both forms parse to the same value.
pub(super) fn to_string<T: Serialize>(value: &T) -> Result<String> {
    let options = serde_saphyr::ser_options! {
        prefer_block_scalars: false,
        yaml_12: true,
        no_lang_directive: true,
    };
    Ok(serde_saphyr::to_string_with_options(value, options)?)
}
