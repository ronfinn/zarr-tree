//! Integration tests: these run the compiled `zarr-tree` binary against
//! throwaway fixture stores and check what it prints.
//!
//! Files under `tests/` are compiled as their own crate, linked against this
//! package from the outside. Since `zarr-tree` is a binary with no library
//! target, nothing inside `src/main.rs` is visible here -- the command line is
//! the whole public surface, and that is exactly what these tests exercise.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fresh, empty directory of our own under the system temp directory.
///
/// The process id separates two `cargo test` runs happening at once; `name`
/// separates the tests inside one run, which share a process but run on
/// different threads. Any leftovers from a crashed earlier run are removed
/// first, so a test never inherits a half-built fixture.
fn fixture_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("zarr-tree-it-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `contents` to `path`, creating the directories above it as needed.
fn write_file(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Run the binary Cargo just built for us, with `args`, and capture its output.
///
/// `CARGO_BIN_EXE_zarr-tree` is set by Cargo when it compiles this test, and
/// `env!` reads it at compile time -- so the path to the executable is a plain
/// string baked into the test. That is what keeps this honest: it runs this
/// build's binary, never whatever happens to be on `PATH`.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zarr-tree"))
        .args(args)
        .output()
        .expect("failed to run the zarr-tree binary")
}

/// Turn raw stdout into comparable lines.
///
/// The tree connectors and the padding that lines up the metadata values are
/// presentation, not meaning. Replacing the box-drawing characters with spaces
/// and then collapsing every run of whitespace leaves `│   ├─ shape:  [4, 4]`
/// as `shape: [4, 4]`, so these tests assert on what the tool says rather than
/// on the exact shape of the drawing.
fn lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|line| {
            line.replace(['├', '└', '│', '─'], " ")
                .split_whitespace()
                .collect::<Vec<&str>>()
                .join(" ")
        })
        .collect()
}

/// Is `needle` somewhere in one of these normalised lines?
///
/// `contains` rather than `==` because the root line carries the full path the
/// command was given, e.g. `/tmp/.../dataset.zarr [group]`.
fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

#[test]
fn prints_a_zarr_tree_with_groups_arrays_and_metadata() {
    let dir = fixture_dir("tree");
    let root = dir.join("dataset.zarr");

    // A V3 group at the root, holding a V3 array and a V2 group.
    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    write_file(
        &root.join("image/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [4096, 4096],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [512, 512] }
            },
            "data_type": "uint16"
        }"#,
    );
    // V3 chunk storage: a `c/` directory with one chunk key beneath it. Empty
    // directories are enough -- the walk only ever looks at directory names.
    fs::create_dir_all(root.join("image/c/0")).unwrap();

    write_file(&root.join("labels/.zgroup"), r#"{"zarr_format": 2}"#);
    write_file(
        &root.join("labels/mask/.zarray"),
        r#"{"zarr_format": 2, "shape": [4096, 4096], "chunks": [512, 512], "dtype": "|u1"}"#,
    );
    // V2 chunk storage: chunk keys sit directly inside the array directory.
    fs::create_dir_all(root.join("labels/mask/0")).unwrap();

    let output = run(&[root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Clean up before asserting: a failing assert panics, and nothing after
    // the panic would run.
    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "expected success, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = lines(&stdout);
    for expected in [
        "dataset.zarr [group]",
        "image [array]",
        "shape: [4096, 4096]",
        "chunks: [512, 512]",
        "dtype: uint16",
        "labels [group]",
        "mask [array]",
        "dtype: |u1",
    ] {
        assert!(has(&lines, expected), "missing {expected:?} in:\n{stdout}");
    }

    // Traversal stops at arrays, so the chunk directories underneath them are
    // never reached. Were they printed they would carry no metadata of their
    // own and so would show up as `[unknown]`; there is nothing else in this
    // fixture that could produce that label.
    assert!(
        !stdout.contains("[unknown]"),
        "chunk storage leaked into the tree:\n{stdout}"
    );
    assert!(!has(&lines, "c [unknown]"));
    assert!(!has(&lines, "0 [unknown]"));

    // Nothing here carries OME-Zarr metadata, so the output is exactly what it
    // was before axes existed.
    assert!(
        !stdout.contains("axes:"),
        "a store with no OME-Zarr metadata should print no axes row:\n{stdout}"
    );
}

#[test]
fn malformed_metadata_is_labelled_unknown_and_the_walk_continues() {
    let dir = fixture_dir("malformed");
    let root = dir.join("broken.zarr");

    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    // Truncated mid-object: serde_json will refuse to parse this.
    write_file(
        &root.join("truncated/zarr.json"),
        r#"{"zarr_format": 3, "node_type":"#,
    );

    let output = run(&[root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "one unreadable file should not fail the run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = lines(&stdout);
    assert!(
        has(&lines, "truncated [unknown]"),
        "expected the malformed child to be [unknown] in:\n{stdout}"
    );
    // The rest of the tree still prints around it.
    assert!(
        has(&lines, "broken.zarr [group]"),
        "expected the root group line in:\n{stdout}"
    );
}

#[test]
fn ome_zarr_image_group_is_tagged() {
    let dir = fixture_dir("ome");
    let root = dir.join("image.zarr");

    // A Zarr V2 image group. `.zgroup` makes it a group; `.zattrs` beside it
    // carries the OME-Zarr metadata, which in V2 sits at the top level with no
    // `ome` namespace around it.
    write_file(&root.join(".zgroup"), r#"{"zarr_format": 2}"#);
    write_file(
        &root.join(".zattrs"),
        r#"{
            "multiscales": [
                {
                    "version": "0.4",
                    "axes": [{"name": "y", "type": "space"}, {"name": "x", "type": "space"}],
                    "datasets": [
                        {"path": "0", "coordinateTransformations": [{"type": "scale", "scale": [1.0, 1.0]}]},
                        {"path": "1", "coordinateTransformations": [{"type": "scale", "scale": [2.0, 2.0]}]}
                    ]
                }
            ]
        }"#,
    );
    // The two resolution levels, as ordinary V2 arrays.
    write_file(
        &root.join("0/.zarray"),
        r#"{"zarr_format": 2, "shape": [1024, 1024], "chunks": [512, 512], "dtype": "<u2"}"#,
    );
    write_file(
        &root.join("1/.zarray"),
        r#"{"zarr_format": 2, "shape": [512, 512], "chunks": [512, 512], "dtype": "<u2"}"#,
    );

    let output = run(&[root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "expected success, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = lines(&stdout);
    for expected in [
        // The tag is appended to the group label, version included.
        "image.zarr [group, OME-Zarr 0.4]",
        // The axes this fixture's `.zattrs` declares, in the object form 0.4
        // uses. The root is an image group, so its row sits at the top level.
        "axes: y, x",
        // The levels are still plain arrays, still carrying their metadata.
        "0 [array]",
        "1 [array]",
        "shape: [1024, 1024]",
        "shape: [512, 512]",
        "chunks: [512, 512]",
        "dtype: <u2",
    ] {
        assert!(has(&lines, expected), "missing {expected:?} in:\n{stdout}");
    }
}

#[test]
fn ome_zarr_axes_are_printed_above_the_datasets() {
    let dir = fixture_dir("ome-axes");
    let root = dir.join("dataset.zarr");

    // A plain V3 group at the root, so the image group below it is reached
    // through the recursive walk rather than as the root itself.
    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );

    // A Zarr V3 / OME-NGFF 0.5 image group: the OME-Zarr keys live under the
    // `ome` namespace inside the group's own `zarr.json`.
    write_file(
        &root.join("image/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        {
                            "axes": [
                                {"name": "c", "type": "channel"},
                                {"name": "y", "type": "space", "unit": "micrometer"},
                                {"name": "x", "type": "space", "unit": "micrometer"}
                            ],
                            "datasets": [
                                {"path": "0", "coordinateTransformations": [{"type": "scale", "scale": [1.0, 1.0, 1.0]}]}
                            ]
                        }
                    ]
                }
            }
        }"#,
    );
    write_file(
        &root.join("image/0/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [2, 64, 64],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [1, 32, 32] }
            },
            "data_type": "uint16"
        }"#,
    );

    // A second image group whose resolution levels are missing entirely -- a
    // broken store, but a readable one. Sorted after "image", so it is the
    // last child and its axes row has nothing below it.
    write_file(
        &root.join("orphan/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        {
                            "axes": [{"name": "y", "type": "space"}, {"name": "x", "type": "space"}],
                            "datasets": [{"path": "0"}]
                        }
                    ]
                }
            }
        }"#,
    );

    let output = run(&[root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "expected success, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = lines(&stdout);
    for expected in [
        "image [group, OME-Zarr 0.5]",
        "axes: c, y, x",
        // The resolution level and its metadata are untouched by the new row.
        "0 [array]",
        "shape: [2, 64, 64]",
        "chunks: [1, 32, 32]",
        "dtype: uint16",
    ] {
        assert!(has(&lines, expected), "missing {expected:?} in:\n{stdout}");
    }

    // The row belongs to the group, so it comes before the group's children.
    let axes_row = lines
        .iter()
        .position(|line| line.contains("axes: c, y, x"))
        .expect("expected an axes row");
    let dataset = lines
        .iter()
        .position(|line| line.contains("0 [array]"))
        .expect("expected the resolution level");
    assert!(
        axes_row < dataset,
        "the axes row should precede the datasets in:\n{stdout}"
    );

    // Indentation and connectors, checked against the undisturbed output:
    // a nested group's row is indented under its parent and keeps the stem
    // running down to the children below it.
    assert!(
        stdout.contains("│   ├─ axes: c, y, x"),
        "expected a nested axes row with children following in:\n{stdout}"
    );
    // With no children below it, the row closes its branch instead.
    assert!(
        stdout.contains("    └─ axes: y, x"),
        "expected a closing axes row on the childless group in:\n{stdout}"
    );
}

#[test]
fn help_flag_prints_usage_and_exits_successfully() {
    let output = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "--help should exit 0");
    for expected in ["zarr-tree", "USAGE", "--help", "--version"] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in --help output:\n{stdout}"
        );
    }
}
