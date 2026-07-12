//! Parser for the strict `name=value` prebuilt bundle manifest.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const KNOWN_FIELDS: &[&str] = &[
    "bzip2_headers_sha256",
    "bindings_sha256",
    "build_script_sha256",
    "compiler",
    "compiler_version",
    "crate_version",
    "cxx_std",
    "cxx_stdlib",
    "deployment_target",
    "extensions_sha256",
    "features",
    "format",
    "headers_sha256",
    "jemalloc_headers_sha256",
    "link",
    "lz4_headers_sha256",
    "rocksdb_sha256",
    "rocksdb_version",
    "snappy_sha256",
    "source_list_sha256",
    "source_revision",
    "target",
    "target_cpu",
    "validator_sha256",
    "zlib_headers_sha256",
    "zstd_headers_sha256",
];

/// Parsed fields from one validated bundle manifest.
pub(super) struct Manifest {
    fields: BTreeMap<String, String>,
}

impl Manifest {
    pub(super) fn load(path: &Path) -> Self {
        let content = fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "cannot read prebuilt RocksDB manifest `{}`: {e}",
                path.display()
            )
        });
        let fields = parse_fields(path, &content);
        validate_known_fields(path, &fields);
        Self { fields }
    }

    pub(super) fn get(&self, name: &str) -> &str {
        self.fields
            .get(name)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("prebuilt RocksDB manifest is missing `{name}`"))
    }
}

fn parse_fields(path: &Path, content: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = parse_field(path, index + 1, line);
        if fields.insert(name.to_owned(), value.to_owned()).is_some() {
            panic!(
                "duplicate prebuilt RocksDB manifest field `{name}` in `{}`",
                path.display()
            );
        }
    }
    fields
}

fn parse_field<'a>(path: &Path, line_number: usize, line: &'a str) -> (&'a str, &'a str) {
    line.split_once('=').unwrap_or_else(|| {
        panic!(
            "invalid prebuilt RocksDB manifest line {line_number} in `{}`: expected name=value, got `{line}`",
            path.display()
        )
    })
}

fn validate_known_fields(path: &Path, fields: &BTreeMap<String, String>) {
    for name in fields.keys() {
        if !KNOWN_FIELDS.contains(&name.as_str()) {
            panic!(
                "unknown prebuilt RocksDB manifest field `{name}` in `{}`",
                path.display()
            );
        }
    }
    for name in KNOWN_FIELDS {
        if !fields.contains_key(*name) {
            panic!(
                "prebuilt RocksDB manifest `{}` is missing `{name}`",
                path.display()
            );
        }
    }
}
