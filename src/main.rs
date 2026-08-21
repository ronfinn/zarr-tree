use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::Value;

/// What kind of Zarr node a directory is, as far as its metadata files reveal.
enum NodeKind {
    /// A Zarr group. The payload is the OME-Zarr tag -- `Some("OME-Zarr 0.4")`,
    /// or `Some("OME-Zarr")` when no version could be read -- and is `None` for
    /// an ordinary group. See `ome_tag`.
    Group(Option<String>),
    Array(ArrayMeta),
    Unknown,
}

/// The three fields shown underneath an array, already formatted for printing.
/// Each is optional on its own, so metadata missing `chunks` still shows the
/// shape it does have.
struct ArrayMeta {
    shape: Option<String>,
    chunks: Option<String>,
    dtype: Option<String>,
}

/// The text printed by `--help`.
///
/// A plain multi-line string literal: everything between the quotes, newlines
/// included, is part of the value. That is why these lines sit flush against
/// the left margin instead of following the indentation around them. The `\`
/// after the opening quote swallows the newline that would otherwise start the
/// string with a blank line.
const HELP: &str = "\
zarr-tree
Explore the structure and metadata of a local Zarr store.

USAGE:
    zarr-tree <DIRECTORY>

OPTIONS:
    -h, --help       Print help
    -V, --version    Print version";

impl NodeKind {
    /// The tag printed after a directory name.
    ///
    /// A group carrying OME-Zarr image metadata says so here, as
    /// `[group, OME-Zarr 0.4]`. That text is built at run time from the version
    /// found in the file, so this can no longer hand back a `&'static str`
    /// borrowing a literal compiled into the binary: it returns an owned
    /// `String` instead.
    fn label(&self) -> String {
        match self {
            NodeKind::Group(None) => String::from("[group]"),
            NodeKind::Group(Some(tag)) => format!("[group, {tag}]"),
            NodeKind::Array(_) => String::from("[array]"),
            NodeKind::Unknown => String::from("[unknown]"),
        }
    }
}

fn main() {
    // args[0] is the program itself, so we expect exactly two entries.
    let args: Vec<String> = env::args().collect();

    // A lone flag is answered before anything else. Both arms return from
    // main, so a flag never reaches the path checks below and every other
    // argument falls through to exactly the code that handled it before.
    //
    // The cost of parsing this simply is that a directory actually named "-h"
    // or "-V" can no longer be inspected.
    if args.len() == 2 {
        // args[1] is a String; as_str() borrows it as a &str so it can be
        // matched against string literals.
        match args[1].as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                // env! reads the variable when the crate is compiled, so this
                // is a plain string literal in the binary. Cargo fills it in
                // from the version field in Cargo.toml, which is why the two
                // cannot drift apart.
                println!("zarr-tree {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {}
        }
    }

    if args.len() != 2 {
        eprintln!("usage: zarr-tree <directory>");
        process::exit(1);
    }

    let root = Path::new(&args[1]);
    if !root.exists() {
        eprintln!("error: path does not exist: {}", root.display());
        process::exit(1);
    }
    if !root.is_dir() {
        eprintln!("error: path is not a directory: {}", root.display());
        process::exit(1);
    }

    let root_name = args[1].trim_end_matches('/');
    let root_kind = classify(root);
    println!("{root_name} {}", root_kind.label());

    // An array is a leaf here too: its metadata takes the place of the walk.
    match &root_kind {
        NodeKind::Array(meta) => print_array_meta(meta, ""),
        _ => {
            if let Err(e) = print_tree(root, "") {
                eprintln!("error: {e}");
                process::exit(1);
            }
        }
    }
}

/// Print the directories inside `dir`, one line each, indented by `prefix`.
fn print_tree(dir: &Path, prefix: &str) -> io::Result<()> {
    // Collect first: read_dir returns entries in arbitrary order, and we need
    // to know which child is last before we can draw its connector.
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // file_type() does not follow symlinks, so a link pointing back at an
        // ancestor cannot send us into infinite recursion.
        if entry.file_type()?.is_dir() {
            subdirs.push(entry.path());
        }
    }
    subdirs.sort();

    for (i, path) in subdirs.iter().enumerate() {
        let is_last = i == subdirs.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let kind = classify(path);
        println!("{prefix}{connector}{name} {}", kind.label());

        // Children of the last entry need no vertical bar above them.
        let child_prefix = if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        // Stop at arrays: what lies beneath is chunk storage (a V3 `c/`
        // directory, V2 chunk keys), an implementation detail rather than
        // structure worth showing. The metadata rows go there instead.
        match &kind {
            NodeKind::Array(meta) => print_array_meta(meta, &child_prefix),
            _ => print_tree(path, &child_prefix)?,
        }
    }

    Ok(())
}

/// Decide what kind of Zarr node `dir` is by looking at the metadata files
/// directly inside it.
///
/// Classification never fails: a directory we cannot read, or whose metadata we
/// do not understand, is simply `Unknown`. A broken `zarr.json` in one corner of
/// the tree should not abort the whole walk.
fn classify(dir: &Path) -> NodeKind {
    // Zarr V2 keeps the two node kinds in separate files, so the filename alone
    // answers the question -- no need to open anything.
    if dir.join(".zgroup").is_file() {
        return NodeKind::Group(ome_tag_v2(dir));
    }
    let zarray = dir.join(".zarray");
    if zarray.is_file() {
        return NodeKind::Array(array_meta_v2(&zarray));
    }

    // Zarr V3 uses one filename for both kinds and moves the distinction inside
    // the file, so here we do have to read it. Checked second, so a store that
    // carries both V2 and V3 metadata is reported as V2.
    if let Some(kind) = classify_v3(&dir.join("zarr.json")) {
        return kind;
    }

    NodeKind::Unknown
}

/// Read `node_type` out of a Zarr V3 `zarr.json`, or `None` if the file is
/// missing, unreadable, not valid JSON, or has no recognisable `node_type`.
fn classify_v3(path: &Path) -> Option<NodeKind> {
    let value = read_json(path)?;

    match value.get("node_type")?.as_str()? {
        "group" => Some(NodeKind::Group(ome_tag_v3(&value))),
        "array" => Some(NodeKind::Array(array_meta_v3(&value))),
        _ => None,
    }
}

/// Look for OME-Zarr image metadata beside a Zarr V2 `.zgroup`.
///
/// V2 keeps user attributes in a `.zattrs` file separate from `.zgroup`, so
/// this is the one extra read the tag costs us. A `.zattrs` that is missing or
/// unparseable simply means no tag.
fn ome_tag_v2(dir: &Path) -> Option<String> {
    let attrs = read_json(&dir.join(".zattrs"))?;

    // V2 predates the `ome` namespace: the keys sit at the top level of
    // `.zattrs`, and the version belongs to an individual multiscale rather
    // than to the group. Real 0.4 stores often leave it out entirely.
    let version = attrs
        .get("multiscales")
        .and_then(|value| value.as_array())
        .and_then(|entries| entries.first())
        .and_then(|first| first.get("version"));

    ome_tag(&attrs, version)
}

/// Look for OME-Zarr image metadata in an already-parsed Zarr V3 `zarr.json`.
///
/// V3 keeps group attributes in that same file, so nothing extra is read here.
/// The OME-Zarr keys live under a namespace of their own, version included.
fn ome_tag_v3(value: &Value) -> Option<String> {
    let ome = value.get("attributes")?.get("ome")?;
    ome_tag(ome, ome.get("version"))
}

/// Build the tag appended to an OME-Zarr image group's label, or `None` if this
/// is an ordinary Zarr group.
///
/// `ome` is whichever object holds the OME-Zarr keys -- the whole `.zattrs` for
/// V2, `attributes.ome` for V3 -- and `version` is passed in separately because
/// that is the only other thing the two layouts disagree about.
fn ome_tag(ome: &Value, version: Option<&Value>) -> Option<String> {
    // A `multiscales` key holding a non-empty array is what makes a group an
    // image. Missing, the wrong JSON type, or empty all mean it is not one, and
    // `?` turns the first two into `None` without any error handling of ours.
    let multiscales = ome.get("multiscales")?.as_array()?;
    if multiscales.is_empty() {
        return None;
    }

    // Shown exactly as stored, and never checked against the versions we happen
    // to know about: this tool reports metadata rather than validating it. A
    // version that is absent, or is not a string, just leaves the tag bare.
    match version.and_then(|value| value.as_str()) {
        Some(version) => Some(format!("OME-Zarr {version}")),
        None => Some(String::from("OME-Zarr")),
    }
}

/// Read a file and parse it as JSON, or `None` if either step fails.
fn read_json(path: &Path) -> Option<Value> {
    // `.ok()` drops the error and leaves an Option: we care *that* this failed,
    // not why. `?` then returns None early, the same way it returns Err early
    // on a Result.
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Collect the display metadata from a Zarr V2 `.zarray`.
///
/// The filename already told us this is an array, so a file we cannot read or
/// parse only costs us the metadata: every field comes back missing and the
/// node is still shown as an array.
fn array_meta_v2(path: &Path) -> ArrayMeta {
    let Some(value) = read_json(path) else {
        return ArrayMeta {
            shape: None,
            chunks: None,
            dtype: None,
        };
    };

    ArrayMeta {
        shape: format_dims(value.get("shape")),
        chunks: format_dims(value.get("chunks")),
        // Shown exactly as stored, in V2's NumPy notation ("<u2", "|u1").
        dtype: value
            .get("dtype")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Collect the display metadata from an already-parsed Zarr V3 `zarr.json`.
fn array_meta_v3(value: &Value) -> ArrayMeta {
    // V3 keeps the chunk shape inside the chunk grid description. Only the
    // "regular" grid has a `chunk_shape`; any other grid simply yields None
    // here and the row shows as missing.
    let chunk_shape = value
        .get("chunk_grid")
        .and_then(|grid| grid.get("configuration"))
        .and_then(|config| config.get("chunk_shape"));

    ArrayMeta {
        shape: format_dims(value.get("shape")),
        chunks: format_dims(chunk_shape),
        // The object form used by dtype extensions is not interpreted.
        dtype: value
            .get("data_type")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Render a JSON array of numbers as `[4096, 4096]`, or `None` if the value is
/// missing or is not an array.
fn format_dims(value: Option<&Value>) -> Option<String> {
    let items = value?.as_array()?;
    let dims: Vec<String> = items.iter().map(|item| item.to_string()).collect();
    Some(format!("[{}]", dims.join(", ")))
}

/// Print the metadata rows that sit underneath an array line.
///
/// All three rows are always printed, in the same order, so the closing
/// connector is always on `dtype`. A field we could not read shows as `?`.
fn print_array_meta(meta: &ArrayMeta, prefix: &str) {
    let rows = [
        ("shape:", &meta.shape),
        ("chunks:", &meta.chunks),
        ("dtype:", &meta.dtype),
    ];

    for (i, (name, value)) in rows.into_iter().enumerate() {
        let connector = if i == rows.len() - 1 {
            "└─ "
        } else {
            "├─ "
        };
        let value = value.as_deref().unwrap_or("?");
        // Pad to the width of the longest name, "chunks:", so values line up.
        println!("{prefix}{connector}{name:<7} {value}");
    }
}

#[cfg(test)]
mod tests {
    // `tests` is a child module of the crate root, so it can already see the
    // root's private items. This glob import just brings their names into
    // scope so we can call them unqualified.
    use super::*;
    use serde_json::json;

    #[test]
    fn v2_metadata_is_read_from_a_zarray_file() {
        // A directory of our own inside the system temp directory. The process
        // id keeps two simultaneous test runs from colliding.
        let dir = env::temp_dir().join(format!("zarr-tree-test-v2-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        let zarray = dir.join(".zarray");
        fs::write(
            &zarray,
            r#"{"shape": [4096, 4096], "chunks": [512, 512], "dtype": "<u2"}"#,
        )
        .unwrap();

        let meta = array_meta_v2(&zarray);

        // Clean up before asserting: a failing assert_eq! panics, and anything
        // after the panic would never run.
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(meta.shape, Some(String::from("[4096, 4096]")));
        assert_eq!(meta.chunks, Some(String::from("[512, 512]")));
        // V2 dtype is passed through untouched, in NumPy notation.
        assert_eq!(meta.dtype, Some(String::from("<u2")));
    }

    #[test]
    fn v3_metadata_is_read_from_a_parsed_zarr_json() {
        let value = json!({
            "node_type": "array",
            "shape": [4096, 4096],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [512, 512] }
            },
            "data_type": "uint16"
        });

        let meta = array_meta_v3(&value);

        assert_eq!(meta.shape, Some(String::from("[4096, 4096]")));
        assert_eq!(meta.chunks, Some(String::from("[512, 512]")));
        assert_eq!(meta.dtype, Some(String::from("uint16")));
    }

    #[test]
    fn v3_metadata_missing_fields_become_none() {
        let value = json!({
            "node_type": "array",
            "shape": [4096, 4096]
        });

        let meta = array_meta_v3(&value);

        assert_eq!(meta.shape, Some(String::from("[4096, 4096]")));
        assert_eq!(meta.chunks, None);
        assert_eq!(meta.dtype, None);
    }

    #[test]
    fn format_dims_renders_a_json_array() {
        let value = json!([128, 256, 256]);

        assert_eq!(
            format_dims(Some(&value)),
            Some(String::from("[128, 256, 256]"))
        );
    }

    #[test]
    fn malformed_v3_metadata_classifies_as_unknown() {
        // A name of its own, so two tests running in parallel cannot delete
        // each other's fixtures.
        let dir = env::temp_dir().join(format!("zarr-tree-test-v3-malformed-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        // Truncated mid-object: serde_json will refuse to parse this.
        fs::write(dir.join("zarr.json"), r#"{"zarr_format": 3, "node_type":"#).unwrap();

        let kind = classify(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // One corrupt file should cost us only that node's label, not the
        // walk. `matches!` checks the variant without making NodeKind derive
        // PartialEq and Debug that nothing outside this test would use.
        assert!(matches!(kind, NodeKind::Unknown));
    }

    #[test]
    fn v2_dtype_is_passed_through_uninterpreted() {
        let dir = env::temp_dir().join(format!("zarr-tree-test-v2-dtype-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        let zarray = dir.join(".zarray");
        // `<M8[ns]` is NumPy's datetime64, valid V2 but outside what a Zarr
        // library would necessarily load. We only display dtypes, so it has
        // to survive to the screen unchanged.
        fs::write(
            &zarray,
            r#"{"zarr_format": 2, "shape": [10], "chunks": [10], "dtype": "<M8[ns]"}"#,
        )
        .unwrap();

        let meta = array_meta_v2(&zarray);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(meta.shape, Some(String::from("[10]")));
        assert_eq!(meta.chunks, Some(String::from("[10]")));
        assert_eq!(meta.dtype, Some(String::from("<M8[ns]")));
    }

    #[test]
    fn ome_version_is_read_from_v3_attributes() {
        // A V3 image group: the OME-Zarr keys sit under their own `ome`
        // namespace inside `attributes`, with the version alongside them.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        { "datasets": [{ "path": "0" }] }
                    ]
                }
            }
        });

        assert_eq!(ome_tag_v3(&value), Some(String::from("OME-Zarr 0.5")));
    }

    #[test]
    fn ome_v2_multiscales_without_version_is_still_detected() {
        let dir = env::temp_dir().join(format!("zarr-tree-test-ome-v2-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        // A V2 image group, written the way real 0.4 stores often are: the keys
        // at the top level of `.zattrs`, and no `version` anywhere. The group is
        // still an OME-Zarr image, so it earns a tag -- just a bare one.
        fs::write(
            dir.join(".zattrs"),
            r#"{"multiscales": [{"datasets": [{"path": "0"}]}]}"#,
        )
        .unwrap();

        let tag = ome_tag_v2(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(tag, Some(String::from("OME-Zarr")));
    }

    #[test]
    fn plain_group_is_not_ome_zarr() {
        // Attributes are present, and so is the `ome` namespace, but there is no
        // `multiscales` -- so this is not an image and gets no tag at all.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": { "version": "0.5" }
            }
        });

        assert_eq!(ome_tag_v3(&value), None);
    }
}
