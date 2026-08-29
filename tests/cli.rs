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
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

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
    // was before any of the OME rows existed.
    for unexpected in ["axes:", "pyramid levels:", "datasets:"] {
        assert!(
            !stdout.contains(unexpected),
            "a store with no OME-Zarr metadata should print no {unexpected:?} row:\n{stdout}"
        );
    }
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
        // The pyramid as the metadata declares it. V2 lays `datasets` out the
        // same way V3 does, so the rows are identical either side.
        "pyramid levels: 2",
        "datasets: 0, 1",
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
fn ome_zarr_metadata_rows_are_printed_above_the_child_arrays() {
    let dir = fixture_dir("ome-rows");
    let root = dir.join("dataset.zarr");

    // A plain V3 group at the root, so the image group below it is reached
    // through the recursive walk rather than as the root itself.
    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );

    // A Zarr V3 / OME-NGFF 0.5 image group: the OME-Zarr keys live under the
    // `ome` namespace inside the group's own `zarr.json`. Two resolution
    // levels, declared and present.
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
                                {"path": "0", "coordinateTransformations": [{"type": "scale", "scale": [1.0, 1.0, 1.0]}]},
                                {"path": "1", "coordinateTransformations": [{"type": "scale", "scale": [1.0, 2.0, 2.0]}]}
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
    write_file(
        &root.join("image/1/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [2, 32, 32],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [1, 32, 32] }
            },
            "data_type": "uint16"
        }"#,
    );

    // A second image group whose resolution levels are missing entirely -- a
    // broken store, but a readable one. Sorted after "image", so it is the
    // last child and its rows have nothing below them. It still reports the
    // level its metadata declares: nothing here checks that the path exists.
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
        "pyramid levels: 2",
        "datasets: 0, 1",
        // The resolution levels and their metadata are untouched by the new
        // rows above them.
        "0 [array]",
        "1 [array]",
        "shape: [2, 64, 64]",
        "chunks: [1, 32, 32]",
        "dtype: uint16",
    ] {
        assert!(has(&lines, expected), "missing {expected:?} in:\n{stdout}");
    }

    // The rows belong to the group, so they all come before the group's
    // children, in the order axes, level count, level paths.
    let row = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("expected a {needle:?} row in:\n{stdout}"))
    };
    let axes = row("axes: c, y, x");
    let levels = row("pyramid levels: 2");
    let datasets = row("datasets: 0, 1");
    let first_child = row("0 [array]");
    assert!(
        axes < levels && levels < datasets && datasets < first_child,
        "expected axes, pyramid levels, datasets, then the child arrays in:\n{stdout}"
    );

    // Indentation and connectors, checked against the undisturbed output: a
    // nested group's rows are indented under it and keep the stem running down
    // to the children below them, so only node rows close the branch.
    for expected in [
        "│   ├─ axes: c, y, x",
        "│   ├─ pyramid levels: 2",
        "│   ├─ datasets: 0, 1",
    ] {
        assert!(
            stdout.contains(expected),
            "expected {expected:?} with children following in:\n{stdout}"
        );
    }
    // With no children below it, the *last* row closes the branch instead --
    // and only the last one.
    assert!(
        stdout.contains("    ├─ axes: y, x"),
        "a non-final row should stay open even with no children in:\n{stdout}"
    );
    assert!(
        stdout.contains("    └─ datasets: 0"),
        "expected a closing datasets row on the childless group in:\n{stdout}"
    );
}

#[test]
fn a_labels_group_is_a_child_but_not_a_pyramid_level() {
    let dir = fixture_dir("ome-labels");
    let root = dir.join("image.zarr");

    // A V2 / OME-NGFF 0.4 image group declaring three resolution levels.
    write_file(&root.join(".zgroup"), r#"{"zarr_format": 2}"#);
    write_file(
        &root.join(".zattrs"),
        r#"{
            "multiscales": [
                {
                    "version": "0.4",
                    "axes": [{"name": "y", "type": "space"}, {"name": "x", "type": "space"}],
                    "datasets": [{"path": "0"}, {"path": "1"}, {"path": "2"}]
                }
            ]
        }"#,
    );
    for (level, size) in [("0", 1024), ("1", 512), ("2", 256)] {
        write_file(
            &root.join(level).join(".zarray"),
            &format!(
                r#"{{"zarr_format": 2, "shape": [{size}, {size}], "chunks": [256, 256], "dtype": "|u1"}}"#
            ),
        );
    }

    // ...and a fourth child directory that is not one of them. A `labels`
    // group beside the levels is ordinary in a real OME-Zarr image, which is
    // exactly what makes counting directories the wrong way to get the level
    // count.
    write_file(&root.join("labels/.zgroup"), r#"{"zarr_format": 2}"#);

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

    // The fixture is only meaningful while it really does have four children,
    // so check that before checking the count it is meant to disagree with.
    assert!(
        has(&lines, "labels [group]"),
        "expected the labels group to be listed as a child in:\n{stdout}"
    );
    let arrays = lines.iter().filter(|line| line.contains("[array]")).count();
    assert_eq!(arrays, 3, "expected three level arrays in:\n{stdout}");

    // Three declared levels, from `multiscales[0].datasets` -- not the four
    // directories on disk.
    assert!(
        has(&lines, "pyramid levels: 3"),
        "expected three declared levels in:\n{stdout}"
    );
    assert!(
        !has(&lines, "pyramid levels: 4"),
        "the levels were counted from the filesystem, not the metadata:\n{stdout}"
    );

    // And `labels` is absent from the declared paths, for the same reason.
    let datasets = lines
        .iter()
        .find(|line| line.contains("datasets:"))
        .unwrap_or_else(|| panic!("expected a datasets row in:\n{stdout}"));
    assert!(
        datasets.ends_with("datasets: 0, 1, 2"),
        "expected exactly the declared paths, got {datasets:?} in:\n{stdout}"
    );
}

#[test]
fn spatialdata_root_is_tagged_from_metadata_not_from_directory_names() {
    // Two stores laid out the same way, differing only in their root
    // metadata. Running the binary over both in one test is what makes the
    // point: the directory names are identical on either side, so anything
    // that tells them apart has to have come from the metadata.
    let dir = fixture_dir("spatialdata");

    // A genuine SpatialData store. The marker carries the container format
    // version, which is shown, and the writing software's version, which is
    // what marks this out as the root rather than an element.
    let store = dir.join("experiment.zarr");
    write_file(
        &store.join("zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.3"
                }
            }
        }"#,
    );

    // The element containers of a real store carry no attributes at all,
    // which is exactly why their names cannot be what tags them.
    let containers = ["images", "labels", "points", "shapes", "tables"];
    for container in containers {
        write_file(
            &store.join(container).join("zarr.json"),
            r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
        );
    }

    // An image element: an OME-Zarr group that also carries SpatialData's
    // mark, a `spatialdata_attrs` holding only its own encoding version. That
    // pairing is what makes it an element; it must not be taken for a second
    // store root, and its OME-Zarr rows must survive being classified.
    write_file(
        &store.join("images/morphology/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5-dev-spatialdata",
                    "multiscales": [
                        {
                            "axes": [
                                { "name": "c", "type": "channel" },
                                { "name": "y", "type": "space" },
                                { "name": "x", "type": "space" }
                            ],
                            "datasets": [{ "path": "s0" }]
                        }
                    ]
                },
                "spatialdata_attrs": { "version": "0.3" }
            }
        }"#,
    );
    write_file(
        &store.join("images/morphology/s0/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [4, 2048, 2048],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [1, 512, 512] }
            },
            "data_type": "uint16"
        }"#,
    );

    // A labels element, alike in every way this tool reads except for the
    // `image-label` object beside its `multiscales`. That key is the whole
    // difference between the two rasters; the `labels/` directory it sits in
    // is not consulted.
    write_file(
        &store.join("labels/nuclei/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5-dev-spatialdata",
                    "image-label": { "version": "0.5" },
                    "multiscales": [
                        {
                            "axes": [
                                { "name": "y", "type": "space" },
                                { "name": "x", "type": "space" }
                            ],
                            "datasets": [{ "path": "s0" }]
                        }
                    ]
                },
                "spatialdata_attrs": { "version": "0.3" }
            }
        }"#,
    );

    // A table element, with the two keys a real one carries: AnnData's own,
    // and SpatialData's beside it.
    write_file(
        &store.join("tables/table/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "anndata",
                "encoding-version": "0.1.0",
                "spatialdata-encoding-type": "ngff:regions_table",
                "region": "nuclei",
                "region_key": "region",
                "instance_key": "instance_id",
                "version": "0.2"
            }
        }"#,
    );
    // Two nodes from inside that table's AnnData subtree. Recognising the
    // table does not stop the walk: it is still an ordinary Zarr hierarchy,
    // and the group above says nothing about what is or is not worth showing
    // below it. Collapsing it would be a separate decision from naming it.
    write_file(
        &store.join("tables/table/obs/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "encoding-type": "dataframe", "encoding-version": "0.2.0" }
        }"#,
    );
    write_file(
        &store.join("tables/table/X/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [8, 4],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [8, 4] }
            },
            "data_type": "float32"
        }"#,
    );

    // The same tree without the marker: an ordinary Zarr store whose children
    // happen to be named after the SpatialData element types.
    let plain = dir.join("plain.zarr");
    write_file(
        &plain.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
    );
    for container in containers {
        write_file(
            &plain.join(container).join("zarr.json"),
            r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
        );
    }

    let store_output = run(&[store.to_str().unwrap()]);
    let store_stdout = String::from_utf8_lossy(&store_output.stdout).into_owned();
    let plain_output = run(&[plain.to_str().unwrap()]);
    let plain_stdout = String::from_utf8_lossy(&plain_output.stdout).into_owned();

    // Clean up before asserting: a failing assert panics, and nothing after
    // the panic would run.
    fs::remove_dir_all(&dir).unwrap();

    assert!(
        store_output.status.success() && plain_output.status.success(),
        "expected both runs to succeed; stderr: {} {}",
        String::from_utf8_lossy(&store_output.stderr),
        String::from_utf8_lossy(&plain_output.stderr)
    );

    let store_lines = lines(&store_stdout);
    for expected in [
        // The root, tagged from its own metadata.
        "experiment.zarr [group, SpatialData 0.2]",
        // The containers, untagged, because their metadata says nothing.
        "images [group]",
        "labels [group]",
        "points [group]",
        "shapes [group]",
        "tables [group]",
        // The two rasters, told apart by their metadata alone. Both keep
        // every OME-Zarr row they had before they were classified.
        "morphology [group, OME-Zarr 0.5-dev-spatialdata, SpatialData image]",
        "nuclei [group, OME-Zarr 0.5-dev-spatialdata, SpatialData labels]",
        "axes: c, y, x",
        "axes: y, x",
        "pyramid levels: 1",
        "datasets: s0",
        "s0 [array]",
        "shape: [4, 2048, 2048]",
        "chunks: [1, 512, 512]",
        "dtype: uint16",
        // The table, named but not collapsed: its AnnData subtree is still an
        // ordinary Zarr hierarchy and is still walked.
        "table [group, SpatialData table]",
        "obs [group]",
        "X [array]",
        "shape: [8, 4]",
        "dtype: float32",
    ] {
        assert!(
            has(&store_lines, expected),
            "missing {expected:?} in:\n{store_stdout}"
        );
    }

    // The root, its two rasters and its table, and nothing else: not the
    // containers whose names match the element types, not the arrays below
    // the rasters, and not the AnnData nodes below the table.
    assert_eq!(
        store_stdout.matches("SpatialData").count(),
        4,
        "expected the root and its three elements to be tagged:\n{store_stdout}"
    );

    // The same tree, without the marker, is just a Zarr store.
    let plain_lines = lines(&plain_stdout);
    assert!(
        has(&plain_lines, "plain.zarr [group]"),
        "expected an untagged root in:\n{plain_stdout}"
    );
    assert!(
        !plain_stdout.contains("SpatialData"),
        "directory names alone must not make a SpatialData store:\n{plain_stdout}"
    );
}

#[test]
fn spatialdata_elements_are_tagged_but_their_containers_are_not() {
    // A store laid out the way a real one is: a marked root, four unmarked
    // container groups, and the elements themselves inside them. What this
    // test is really about is which lines get a tag and which do not.
    let dir = fixture_dir("spatialdata-elements");
    let store = dir.join("experiment.zarr");

    write_file(
        &store.join("zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.1"
                }
            }
        }"#,
    );

    for container in ["points", "shapes"] {
        write_file(
            &store.join(container).join("zarr.json"),
            r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
        );
    }

    // A points element, with the attributes a current Xenium store writes.
    write_file(
        &store.join("points/transcripts/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "ngff:points",
                "axes": ["x", "y", "z"],
                "coordinateTransformations": [],
                "spatialdata_attrs": {
                    "instance_key": "cell_id",
                    "feature_key": "feature_name",
                    "version": "0.2"
                }
            }
        }"#,
    );

    // The payload beside it. Points are written as a partitioned Parquet
    // dataset, so this is a directory rather than a file. It is not a Zarr
    // node and does not appear in the tree as one -- but that is because the
    // element's own metadata said this is a points element and this is where a
    // points element's payload lives, not because of what the directory is
    // called.
    //
    // The part is not Parquet, so there is no summary to print either. The
    // element is still an element: an unreadable payload costs its rows and
    // nothing else.
    write_file(
        &store.join("points/transcripts/points.parquet/part.0.parquet"),
        "not read",
    );

    write_file(
        &store.join("shapes/cell_boundaries/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "ngff:shapes",
                "axes": ["x", "y"],
                "coordinateTransformations": [],
                "spatialdata_attrs": { "version": "0.3" }
            }
        }"#,
    );

    // Shapes are written as a single Parquet file, so this one never appears
    // in the tree: only directories are walked. The asymmetry with
    // points.parquet above is on disk, not in this tool.
    write_file(
        &store.join("shapes/cell_boundaries/shapes.parquet"),
        "not read",
    );

    let output = run(&[store.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "expected success; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output_lines = lines(&stdout);
    for expected in [
        // The root, tagged with the container version, exactly as before.
        "experiment.zarr [group, SpatialData 0.2]",
        // The containers, untagged: their names are not evidence.
        "points [group]",
        "shapes [group]",
        // The elements, tagged with their kind and no version.
        "transcripts [group, SpatialData points]",
        "cell_boundaries [group, SpatialData shapes]",
    ] {
        assert!(
            has(&output_lines, expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }

    // The payload directory is the element's data, not a node beneath it, so
    // it is not walked into and not drawn.
    assert!(
        !stdout.contains("points.parquet"),
        "the payload directory is not a child node:\n{stdout}"
    );

    // Neither payload is readable Parquet, so neither element gained a row.
    // That is the graceful case: the Zarr metadata still classified them.
    assert!(!stdout.contains("rows:"), "{stdout}");

    // The root plus the two elements, and nothing else: not the containers.
    assert_eq!(
        stdout.matches("SpatialData").count(),
        3,
        "expected the root and its two elements to be tagged, and nothing else:\n{stdout}"
    );

    // The single-file payload is not a directory, so the walk never sees it.
    assert!(
        !stdout.contains("shapes.parquet"),
        "a file is not a node in this tree:\n{stdout}"
    );
}

#[test]
fn json_output_says_the_same_things_the_tree_says() {
    // One store carrying every kind of fact the two outputs share: a
    // SpatialData root, an OME-Zarr image element under it, that image's
    // resolution array, and a node whose metadata cannot be read at all.
    let dir = fixture_dir("json");
    let root = dir.join("experiment.zarr");

    write_file(
        &root.join("zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.1"
                }
            }
        }"#,
    );
    write_file(
        &root.join("images/zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
    );
    write_file(
        &root.join("images/morphology/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        {
                            "axes": [{ "name": "y" }, { "name": "x" }],
                            "datasets": [{ "path": "s0" }, { "path": "s1" }]
                        }
                    ]
                },
                "spatialdata_attrs": { "version": "0.3" }
            }
        }"#,
    );
    write_file(
        &root.join("images/morphology/s0/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [2048, 2048],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [512, 512] }
            },
            "data_type": "uint16"
        }"#,
    );
    // An array whose metadata says nothing but its shape, so the two
    // unreadable fields have somewhere to show up as null.
    write_file(
        &root.join("images/morphology/s1/.zarray"),
        r#"{"zarr_format": 2, "shape": [1024, 1024]}"#,
    );
    // And a node that is not a Zarr node at all.
    write_file(&root.join("misc/zarr.json"), r#"{"zarr_format": 3,"#);

    let path = root.to_str().unwrap().to_string();
    let output = run(&["--json", &path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let text = String::from_utf8_lossy(&run(&[&path]).stdout).into_owned();
    let shallow = run(&["--json", "--depth", "1", &path]);
    let shallow_stdout = String::from_utf8_lossy(&shallow.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    assert!(
        output.status.success(),
        "expected success; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Valid JSON, and one object at the top rather than a stream of them.
    let tree: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("--json should print valid JSON: {error}\n{stdout}"));

    // The root is named by the path as it was typed, exactly as the tree's
    // first line names it.
    assert_eq!(tree["name"], json!(path));
    assert_eq!(tree["kind"], json!("group"));
    assert_eq!(
        tree["spatialdata"],
        json!({ "kind": "root", "version": "0.2" })
    );
    // A group is not an array, so it has no array section at all.
    assert_eq!(tree.get("array"), None);

    // `children` is always present, so a reader can walk every node the same
    // way. Descending it finds the same nodes the tree indents.
    let child = |node: &Value, name: &str| -> Value {
        node["children"]
            .as_array()
            .unwrap_or_else(|| panic!("{node} should have children"))
            .iter()
            .find(|child| child["name"] == json!(name))
            .unwrap_or_else(|| panic!("no child named {name:?} in {node}"))
            .clone()
    };

    let images = child(&tree, "images");
    // An ordinary container group: no metadata sections of any kind.
    assert_eq!(images.get("ome"), None);
    assert_eq!(images.get("spatialdata"), None);

    let morphology = child(&images, "morphology");
    assert_eq!(
        morphology["ome"],
        json!({
            "tag": "OME-Zarr 0.5",
            // Which of the three OME-Zarr groups this is, so a reader need not
            // unpick the tag to find out.
            "kind": "image",
            "version": "0.5",
            "axes": ["y", "x"],
            // The count is the length of the list beside it, so the two can
            // never disagree -- the same rule the tree's two rows follow.
            "pyramid_levels": 2,
            "datasets": ["s0", "s1"]
        })
    );
    assert_eq!(
        morphology["spatialdata"],
        json!({ "kind": "image", "version": null })
    );

    let s0 = child(&morphology, "s0");
    assert_eq!(s0["kind"], json!("array"));
    // Real JSON arrays, not the `[2048, 2048]` text the tree draws.
    assert_eq!(
        s0["array"],
        json!({ "shape": [2048, 2048], "chunks": [512, 512], "dtype": "uint16" })
    );
    // Arrays are leaves, and say so with an empty list rather than by omission.
    assert_eq!(s0["children"], json!([]));

    // What the tree prints as `?` is null here, consistently.
    let s1 = child(&morphology, "s1");
    assert_eq!(
        s1["array"],
        json!({ "shape": [1024, 1024], "chunks": null, "dtype": null })
    );

    // A node we cannot classify is `unknown` in both outputs.
    assert_eq!(child(&tree, "misc")["kind"], json!("unknown"));

    // The text output is untouched by any of this.
    let text_lines = lines(&text);
    for expected in [
        "morphology [group, OME-Zarr 0.5, SpatialData image]",
        "axes: y, x",
        "pyramid levels: 2",
        "datasets: s0, s1",
        "s0 [array]",
        "shape: [2048, 2048]",
        "chunks: [512, 512]",
        "dtype: uint16",
        "chunks: ?",
        "dtype: ?",
        "misc [unknown]",
    ] {
        assert!(has(&text_lines, expected), "missing {expected:?}\n{text}");
    }

    // And --json honours --depth, which is the same walk underneath.
    let shallow: Value = serde_json::from_str(&shallow_stdout).expect("valid JSON");
    assert_eq!(child(&shallow, "images")["children"], json!([]));
}

#[test]
fn depth_limits_how_far_the_walk_descends() {
    // Three levels below the root, with an OME-Zarr image at the bottom so
    // that both kinds of metadata row -- a group's and an array's -- have a
    // chance to be cut off with them.
    let dir = fixture_dir("depth");
    let root = dir.join("deep.zarr");

    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    write_file(
        &root.join("outer/zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    write_file(
        &root.join("outer/image/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        {
                            "axes": [{ "name": "y" }, { "name": "x" }],
                            "datasets": [{ "path": "0" }]
                        }
                    ]
                }
            }
        }"#,
    );
    write_file(
        &root.join("outer/image/0/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [64, 64],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [32, 32] }
            },
            "data_type": "uint8"
        }"#,
    );

    let at = |args: &[&str]| {
        let output = run(args);
        assert!(
            output.status.success(),
            "expected success for {args:?}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let path = root.to_str().unwrap().to_string();
    let unlimited = at(&[&path]);
    let depth0 = at(&["--depth", "0", &path]);
    let depth1 = at(&["--depth", "1", &path]);
    let depth2 = at(&["--depth", "2", &path]);
    let depth3 = at(&["--depth", "3", &path]);

    fs::remove_dir_all(&dir).unwrap();

    // The root on its own, and nothing below it.
    assert_eq!(
        depth0.lines().count(),
        1,
        "--depth 0 should print the root alone:\n{depth0}"
    );
    assert!(has(&lines(&depth0), "deep.zarr [group]"));

    // One level: the direct child, and not its child.
    assert!(has(&lines(&depth1), "outer [group]"), "\n{depth1}");
    assert!(!depth1.contains("image"), "\n{depth1}");

    // Two levels: the image group appears, and its own metadata rows come
    // with it -- those describe the node itself, not anything below it.
    let depth2_lines = lines(&depth2);
    for expected in [
        "image [group, OME-Zarr 0.5]",
        "axes: y, x",
        "pyramid levels: 1",
        "datasets: 0",
    ] {
        assert!(
            has(&depth2_lines, expected),
            "missing {expected:?}\n{depth2}"
        );
    }
    // But the array below it is one level too far.
    assert!(!depth2.contains("[array]"), "\n{depth2}");

    // Three levels: the array itself, and an array is a leaf at any depth --
    // its three rows print in full, as they always do.
    let depth3_lines = lines(&depth3);
    for expected in [
        "0 [array]",
        "shape: [64, 64]",
        "chunks: [32, 32]",
        "dtype: uint8",
    ] {
        assert!(
            has(&depth3_lines, expected),
            "missing {expected:?}\n{depth3}"
        );
    }

    // And with no option at all the output is exactly what it always was: a
    // depth deeper than the store changes nothing.
    assert_eq!(
        unlimited, depth3,
        "an unlimited walk and a walk deeper than the store should agree"
    );
}

#[test]
fn a_reader_that_stops_reading_ends_the_run_quietly() {
    // What `zarr-tree store.zarr | head` does, without needing `head`: give
    // the process a pipe, then close our end of it and never read a byte.
    //
    // The fixture has to be big enough that the tool cannot possibly finish
    // writing before we close, or the test would pass without ever exercising
    // the failure. A pipe buffer holds about 64 KiB; 800 arrays print four
    // lines each, some 90 KiB, so the writes must block until someone reads
    // and no-one ever will.
    let dir = fixture_dir("broken-pipe");
    let root = dir.join("big.zarr");
    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    for i in 0..800 {
        write_file(
            &root.join(format!("a{i}")).join("zarr.json"),
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
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_zarr-tree"))
        .arg(root.to_str().unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run the zarr-tree binary");

    // Dropping the read end is what breaks the pipe. `take` moves it out of
    // the child handle so it can be dropped on its own, while we keep the
    // handle itself to wait on.
    drop(child.stdout.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait for the run");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    // Quietly: a panic would print here, and so would an `error:` line.
    assert!(
        stderr.is_empty(),
        "a reader that stopped reading is not an error to report:\n{stderr}"
    );
    // And successfully: the pipeline asked for what it wanted and got it.
    assert!(
        output.status.success(),
        "expected exit status 0, got {:?}",
        output.status
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

#[test]
fn a_sharded_array_reports_its_chunks_and_its_shards_separately() {
    let dir = fixture_dir("sharding");
    let root = dir.join("dataset.zarr");

    write_file(
        &root.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );

    // Two arrays chunked identically at 64x64. One stores those chunks inside
    // 256x256 shards, so its chunk grid describes the shard rather than the
    // chunk -- the two must still agree about what a chunk is.
    write_file(
        &root.join("sharded/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [1024, 1024],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [256, 256]}},
            "codecs": [{
                "name": "sharding_indexed",
                "configuration": {"chunk_shape": [64, 64], "codecs": [{"name": "bytes"}]}
            }],
            "data_type": "uint16"
        }"#,
    );
    write_file(
        &root.join("plain/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "array",
            "shape": [1024, 1024],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [64, 64]}},
            "codecs": [{"name": "bytes"}],
            "data_type": "uint16"
        }"#,
    );

    let path = root.to_str().unwrap().to_string();
    let text = String::from_utf8_lossy(&run(&[&path]).stdout).into_owned();
    let output = run(&["--json", &path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    let rows = lines(&text);
    for expected in ["chunks: [64, 64]", "shards: [256, 256]"] {
        assert!(has(&rows, expected), "expected {expected:?} in:\n{text}");
    }
    // The shard shape is never presented as a chunk shape.
    assert!(
        !has(&rows, "chunks: [256, 256]"),
        "the shard shape leaked into the chunks row:\n{text}"
    );

    // `shards` sits between `chunks` and `dtype`, so `dtype` still closes the
    // block -- for the sharded array and the unsharded one alike.
    assert!(text.contains("├─ shards: [256, 256]"), "{text}");
    assert_eq!(text.matches("└─ dtype:  uint16").count(), 2, "{text}");

    let tree: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("--json should print valid JSON: {error}\n{stdout}"));
    let child = |name: &str| -> Value {
        tree["children"]
            .as_array()
            .expect("the root should have children")
            .iter()
            .find(|child| child["name"] == json!(name))
            .unwrap_or_else(|| panic!("no child named {name:?} in {stdout}"))
            .clone()
    };

    assert_eq!(
        child("sharded")["array"],
        json!({
            "shape": [1024, 1024],
            "chunks": [64, 64],
            "shards": [256, 256],
            "dtype": "uint16"
        })
    );
    // An unsharded array has no shards to miss, so the key is absent rather
    // than null -- null is reserved for a field we looked for and could not
    // read.
    assert_eq!(
        child("plain")["array"],
        json!({ "shape": [1024, 1024], "chunks": [64, 64], "dtype": "uint16" })
    );
    assert_eq!(child("plain")["array"].get("shards"), None);
}

#[test]
fn an_hcs_plate_and_its_wells_are_tagged_from_metadata_not_from_their_names() {
    let dir = fixture_dir("hcs");
    let root = dir.join("plate.ome.zarr");

    // A plate as ome-zarr-py writes one. The row and column groups it declares
    // carry no metadata of their own, which is why only the plate and the wells
    // below them are tagged.
    write_file(
        &root.join("zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {"ome": {"version": "0.5", "plate": {
                "rows": [{"name": "A"}, {"name": "B"}],
                "columns": [{"name": "1"}],
                "wells": [
                    {"path": "A/1", "rowIndex": 0, "columnIndex": 0},
                    {"path": "B/1", "rowIndex": 1, "columnIndex": 0}
                ]
            }}}
        }"#,
    );

    for row in ["A", "B"] {
        // A row group: a plain Zarr group, and nothing more is claimed of it.
        write_file(
            &root.join(row).join("zarr.json"),
            r#"{"zarr_format": 3, "node_type": "group"}"#,
        );
        write_file(
            &root.join(row).join("1/zarr.json"),
            r#"{
                "zarr_format": 3,
                "node_type": "group",
                "attributes": {"ome": {"version": "0.5", "well": {"images": [{"path": "0"}]}}}
            }"#,
        );
        // The field of view inside the well is an ordinary OME-Zarr image, and
        // is still labelled exactly as it was before plates were recognised.
        write_file(
            &root.join(row).join("1/0/zarr.json"),
            r#"{
                "zarr_format": 3,
                "node_type": "group",
                "attributes": {"ome": {"version": "0.5", "multiscales": [{
                    "axes": [{"name": "y"}, {"name": "x"}],
                    "datasets": [{"path": "s0"}]
                }]}}
            }"#,
        );
    }

    // A group named exactly like a well, in a store that never said it was one.
    let decoy = dir.join("decoy.zarr");
    write_file(
        &decoy.join("zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    write_file(
        &decoy.join("A/zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );
    write_file(
        &decoy.join("A/1/zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    );

    let path = root.to_str().unwrap().to_string();
    let text = String::from_utf8_lossy(&run(&[&path]).stdout).into_owned();
    let output = run(&["--json", "--depth", "1", &path]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let decoy_path = decoy.to_str().unwrap().to_string();
    let decoy_text = String::from_utf8_lossy(&run(&[&decoy_path]).stdout).into_owned();

    fs::remove_dir_all(&dir).unwrap();

    let rows = lines(&text);
    // The plate is tagged, and says how big it declares itself to be.
    assert!(
        has(&rows, "plate.ome.zarr [group, OME-Zarr 0.5 plate]"),
        "{text}"
    );
    for expected in ["rows: 2", "columns: 1", "wells: 2"] {
        assert!(has(&rows, expected), "expected {expected:?} in:\n{text}");
    }
    // The row groups claim nothing, the wells are tagged, and the image inside
    // a well keeps the bare label it has always had.
    assert!(has(&rows, "A [group]"), "{text}");
    assert!(has(&rows, "1 [group, OME-Zarr 0.5 well]"), "{text}");
    assert!(has(&rows, "0 [group, OME-Zarr 0.5]"), "{text}");
    // A well is tagged and nothing more.
    assert!(!has(&rows, "images:"), "{text}");

    // Identically named groups with no metadata stay ordinary groups: the
    // names `A` and `1` are not what makes a plate.
    let decoy_rows = lines(&decoy_text);
    assert!(!has(&decoy_rows, "plate"), "{decoy_text}");
    assert!(!has(&decoy_rows, "well"), "{decoy_text}");

    let tree: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("--json should print valid JSON: {error}\n{stdout}"));
    assert_eq!(
        tree["ome"],
        json!({
            "tag": "OME-Zarr 0.5 plate",
            "kind": "plate",
            "version": "0.5",
            "rows": 2,
            "columns": 1,
            "wells": 2,
            // A plate has no multiscale of its own, so the image fields are
            // null the same way any unread field is.
            "axes": null,
            "pyramid_levels": null,
            "datasets": null,
        })
    );
}

/// One Zarr V2 store, written the two ways zarr-python can write it: with a
/// consolidated `.zmetadata` copying every node's metadata, and without.
///
/// The metadata files themselves are identical in the two, and are real
/// zarr-python 3.3.0 output. What differs is only whether the copy is there.
const V2_NODES: &[(&str, &str)] = &[
    (
        ".zattrs",
        r#"{
  "note": "root"
}"#,
    ),
    (
        ".zgroup",
        r#"{
  "zarr_format": 2
}"#,
    ),
    (
        "images/.zattrs",
        r#"{
  "multiscales": [
    {
      "version": "0.4",
      "axes": [
        {
          "name": "y",
          "type": "space"
        },
        {
          "name": "x",
          "type": "space"
        }
      ],
      "datasets": [
        {
          "path": "0"
        },
        {
          "path": "1"
        }
      ]
    }
  ]
}"#,
    ),
    (
        "images/.zgroup",
        r#"{
  "zarr_format": 2
}"#,
    ),
    (
        "images/0/.zarray",
        r#"{
  "shape": [
    64,
    64
  ],
  "chunks": [
    32,
    32
  ],
  "dtype": "|u1",
  "fill_value": 0,
  "order": "C",
  "filters": null,
  "dimension_separator": ".",
  "compressor": {
    "id": "blosc",
    "cname": "lz4",
    "clevel": 5,
    "shuffle": 1,
    "blocksize": 0
  },
  "zarr_format": 2
}"#,
    ),
    ("images/0/.zattrs", r#"{}"#),
    (
        "images/1/.zarray",
        r#"{
  "shape": [
    32,
    32
  ],
  "chunks": [
    16,
    16
  ],
  "dtype": "|u1",
  "fill_value": 0,
  "order": "C",
  "filters": null,
  "dimension_separator": ".",
  "compressor": {
    "id": "blosc",
    "cname": "lz4",
    "clevel": 5,
    "shuffle": 1,
    "blocksize": 0
  },
  "zarr_format": 2
}"#,
    ),
    ("images/1/.zattrs", r#"{}"#),
    ("labels/.zattrs", r#"{}"#),
    (
        "labels/.zgroup",
        r#"{
  "zarr_format": 2
}"#,
    ),
    (
        "labels/mask/.zarray",
        r#"{
  "shape": [
    8,
    8
  ],
  "chunks": [
    4,
    4
  ],
  "dtype": "<i4",
  "fill_value": 0,
  "order": "C",
  "filters": null,
  "dimension_separator": ".",
  "compressor": {
    "id": "blosc",
    "cname": "lz4",
    "clevel": 5,
    "shuffle": 1,
    "blocksize": 0
  },
  "zarr_format": 2
}"#,
    ),
    ("labels/mask/.zattrs", r#"{}"#),
];

/// The consolidated document of that same store, verbatim.
const V2_ZMETADATA: &str = r#"{"metadata": {".zgroup": {"zarr_format": 2}, ".zattrs": {"note": "root"}, "images/.zattrs": {"multiscales": [{"version": "0.4", "axes": [{"name": "y", "type": "space"}, {"name": "x", "type": "space"}], "datasets": [{"path": "0"}, {"path": "1"}]}]}, "images/.zgroup": {"zarr_format": 2, "consolidated_metadata": {"metadata": {}, "must_understand": false, "kind": "inline"}}, "labels/.zattrs": {}, "labels/.zgroup": {"zarr_format": 2, "consolidated_metadata": {"metadata": {}, "must_understand": false, "kind": "inline"}}, "images/0/.zattrs": {}, "images/0/.zarray": {"shape": [64, 64], "chunks": [32, 32], "dtype": "|u1", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}, "images/1/.zattrs": {}, "images/1/.zarray": {"shape": [32, 32], "chunks": [16, 16], "dtype": "|u1", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}, "labels/mask/.zattrs": {}, "labels/mask/.zarray": {"shape": [8, 8], "chunks": [4, 4], "dtype": "<i4", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}}, "zarr_consolidated_format": 1}"#;

/// One Zarr V3 store's per-node metadata, without its root.
const V3_NODES: &[(&str, &str)] = &[
    (
        "a/b/arr/zarr.json",
        r#"{
  "shape": [
    4
  ],
  "data_type": "float32",
  "chunk_grid": {
    "name": "regular",
    "configuration": {
      "chunk_shape": [
        2
      ]
    }
  },
  "chunk_key_encoding": {
    "name": "default",
    "configuration": {
      "separator": "/"
    }
  },
  "fill_value": 0.0,
  "codecs": [
    {
      "name": "bytes",
      "configuration": {
        "endian": "little"
      }
    },
    {
      "name": "zstd",
      "configuration": {
        "level": 0,
        "checksum": false
      }
    }
  ],
  "attributes": {},
  "zarr_format": 3,
  "node_type": "array",
  "storage_transformers": []
}"#,
    ),
    (
        "a/b/zarr.json",
        r#"{
  "attributes": {},
  "zarr_format": 3,
  "node_type": "group"
}"#,
    ),
    (
        "a/zarr.json",
        r#"{
  "attributes": {},
  "zarr_format": 3,
  "node_type": "group"
}"#,
    ),
    (
        "images/0/zarr.json",
        r#"{
  "shape": [
    64,
    64
  ],
  "data_type": "uint16",
  "chunk_grid": {
    "name": "regular",
    "configuration": {
      "chunk_shape": [
        32,
        32
      ]
    }
  },
  "chunk_key_encoding": {
    "name": "default",
    "configuration": {
      "separator": "/"
    }
  },
  "fill_value": 0,
  "codecs": [
    {
      "name": "bytes",
      "configuration": {
        "endian": "little"
      }
    },
    {
      "name": "zstd",
      "configuration": {
        "level": 0,
        "checksum": false
      }
    }
  ],
  "attributes": {},
  "zarr_format": 3,
  "node_type": "array",
  "storage_transformers": []
}"#,
    ),
    (
        "images/zarr.json",
        r#"{
  "attributes": {
    "ome": {
      "version": "0.5",
      "multiscales": [
        {
          "axes": [
            {
              "name": "y",
              "type": "space"
            },
            {
              "name": "x",
              "type": "space"
            }
          ],
          "datasets": [
            {
              "path": "0"
            }
          ]
        }
      ]
    }
  },
  "zarr_format": 3,
  "node_type": "group"
}"#,
    ),
];

/// That store's root, with the inline `consolidated_metadata` block that
/// carries the whole hierarchy.
const V3_ROOT_CONSOLIDATED: &str = r#"{
  "attributes": {
    "note": "root3"
  },
  "zarr_format": 3,
  "consolidated_metadata": {
    "kind": "inline",
    "must_understand": false,
    "metadata": {
      "a": {
        "attributes": {},
        "zarr_format": 3,
        "consolidated_metadata": {
          "kind": "inline",
          "must_understand": false,
          "metadata": {}
        },
        "node_type": "group"
      },
      "images": {
        "attributes": {
          "ome": {
            "version": "0.5",
            "multiscales": [
              {
                "axes": [
                  {
                    "name": "y",
                    "type": "space"
                  },
                  {
                    "name": "x",
                    "type": "space"
                  }
                ],
                "datasets": [
                  {
                    "path": "0"
                  }
                ]
              }
            ]
          }
        },
        "zarr_format": 3,
        "consolidated_metadata": {
          "kind": "inline",
          "must_understand": false,
          "metadata": {}
        },
        "node_type": "group"
      },
      "a/b": {
        "attributes": {},
        "zarr_format": 3,
        "consolidated_metadata": {
          "kind": "inline",
          "must_understand": false,
          "metadata": {}
        },
        "node_type": "group"
      },
      "images/0": {
        "shape": [
          64,
          64
        ],
        "data_type": "uint16",
        "chunk_grid": {
          "name": "regular",
          "configuration": {
            "chunk_shape": [
              32,
              32
            ]
          }
        },
        "chunk_key_encoding": {
          "name": "default",
          "configuration": {
            "separator": "/"
          }
        },
        "fill_value": 0,
        "codecs": [
          {
            "name": "bytes",
            "configuration": {
              "endian": "little"
            }
          },
          {
            "name": "zstd",
            "configuration": {
              "level": 0,
              "checksum": false
            }
          }
        ],
        "attributes": {},
        "zarr_format": 3,
        "node_type": "array",
        "storage_transformers": []
      },
      "a/b/arr": {
        "shape": [
          4
        ],
        "data_type": "float32",
        "chunk_grid": {
          "name": "regular",
          "configuration": {
            "chunk_shape": [
              2
            ]
          }
        },
        "chunk_key_encoding": {
          "name": "default",
          "configuration": {
            "separator": "/"
          }
        },
        "fill_value": 0.0,
        "codecs": [
          {
            "name": "bytes",
            "configuration": {
              "endian": "little"
            }
          },
          {
            "name": "zstd",
            "configuration": {
              "level": 0,
              "checksum": false
            }
          }
        ],
        "attributes": {},
        "zarr_format": 3,
        "node_type": "array",
        "storage_transformers": []
      }
    }
  },
  "node_type": "group"
}"#;

/// The same root with the block removed: the store as it was before
/// `zarr.consolidate_metadata` was called on it.
const V3_ROOT_PLAIN: &str = r#"{
  "attributes": {
    "note": "root3"
  },
  "zarr_format": 3,
  "node_type": "group"
}"#;

/// Write a store from a list of (path, contents) pairs.
fn write_store(root: &Path, files: &[(&str, &str)]) {
    for (key, body) in files {
        write_file(&root.join(key), body);
    }
}

#[test]
fn a_consolidated_store_gives_the_same_json_as_the_same_store_without_it() {
    // The claim consolidation has to earn: it is a faster way to the same
    // answer, not a different answer. Both stores hold identical per-node
    // metadata; only one of them also carries the copy.
    //
    // Compared as parsed JSON rather than as text, and with the root's name
    // dropped, because the two stores necessarily sit in different
    // directories. Everything else -- every node, its kind, its shape, its
    // chunks, its OME-Zarr tag, and the order the children come in -- must
    // match exactly.
    let dir = fixture_dir("consolidated-json");

    for (name, nodes, root_file, extra) in [
        ("v2", V2_NODES, None, Some((".zmetadata", V2_ZMETADATA))),
        (
            "v3",
            V3_NODES,
            Some(("zarr.json", V3_ROOT_CONSOLIDATED)),
            None,
        ),
    ] {
        let consolidated = dir.join(format!("{name}-consolidated.zarr"));
        let plain = dir.join(format!("{name}-plain.zarr"));
        write_store(&consolidated, nodes);
        write_store(&plain, nodes);

        // The one file that separates the two stores.
        match (root_file, extra) {
            // V3 keeps the block inside the root document, so the two roots
            // differ and there is no extra file at all.
            (Some((key, body)), None) => {
                write_file(&consolidated.join(key), body);
                write_file(&plain.join(key), V3_ROOT_PLAIN);
            }
            // V2 keeps it in a file of its own, so the plain store is simply
            // the same store without that file.
            (None, Some((key, body))) => write_file(&consolidated.join(key), body),
            _ => unreachable!("one form or the other"),
        }

        for depth in ["1", "2", "99"] {
            let mut left = json_of(&["--json", "--depth", depth, &consolidated.to_string_lossy()]);
            let mut right = json_of(&["--json", "--depth", depth, &plain.to_string_lossy()]);
            left["name"] = json!("store");
            right["name"] = json!("store");
            assert_eq!(left, right, "{name} at --depth {depth}");
        }
    }
}

/// The parsed `--json` output of one run, which must have succeeded.
fn json_of(args: &[&str]) -> Value {
    let output = run(args);
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

#[test]
fn a_spatialdata_table_is_summarised_from_metadata_at_every_depth() {
    // The rows exist to make a table useful before anybody walks into it, so
    // what this test is really about is what `--depth 1` shows -- and that
    // `--depth 2` still shows everything underneath.
    let dir = fixture_dir("anndata-table");
    let store = dir.join("experiment.zarr");

    write_file(
        &store.join("zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.1"
                }
            }
        }"#,
    );

    write_file(
        &store.join("tables/zarr.json"),
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {}}"#,
    );

    // The table group, as SpatialData's writer leaves it: AnnData's own two
    // keys, the element kind one key over, and the three that say what the
    // table annotates.
    write_file(
        &store.join("tables/table/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "anndata",
                "encoding-version": "0.1.0",
                "spatialdata-encoding-type": "ngff:regions_table",
                "region": "cell_circles",
                "region_key": "region",
                "instance_key": "cell_id",
                "version": "0.2"
            }
        }"#,
    );

    write_file(
        &store.join("tables/table/X/zarr.json"),
        r#"{
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "shape": [167780, 313],
                "encoding-type": "csr_matrix",
                "encoding-version": "0.1.0"
            }
        }"#,
    );

    // The three arrays a CSR matrix is made of, with a chunk beneath one of
    // them. None of the three is opened, and the chunk least of all.
    for name in ["data", "indices", "indptr"] {
        write_file(
            &store.join("tables/table/X").join(name).join("zarr.json"),
            r#"{
                "zarr_format": 3,
                "node_type": "array",
                "shape": [23409569],
                "data_type": "float32",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [131072]}}
            }"#,
        );
    }
    write_file(
        &store.join("tables/table/X/data/c/0"),
        "expression values, and never read",
    );

    for (frame, columns, length) in [
        ("obs", r#"["cell_id", "total_counts", "region"]"#, 167780),
        ("var", r#"["gene_ids"]"#, 313),
    ] {
        write_file(
            &store.join("tables/table").join(frame).join("zarr.json"),
            &format!(
                r#"{{
                    "zarr_format": 3,
                    "node_type": "group",
                    "attributes": {{
                        "column-order": {columns},
                        "_index": "_index",
                        "encoding-type": "dataframe",
                        "encoding-version": "0.2.0"
                    }}
                }}"#
            ),
        );
        write_file(
            &store
                .join("tables/table")
                .join(frame)
                .join("_index/zarr.json"),
            &format!(
                r#"{{
                    "zarr_format": 3,
                    "node_type": "array",
                    "shape": [{length}],
                    "data_type": "string",
                    "chunk_grid": {{"name": "regular", "configuration": {{"chunk_shape": [{length}]}}}}
                }}"#
            ),
        );
        write_file(
            &store.join("tables/table").join(frame).join("_index/c/0"),
            "index values, and never read",
        );
    }

    let path = store.to_string_lossy().into_owned();

    // Two levels down is where the table's own line is, and the summary comes
    // with it: the whole shape of the table before a single child is listed.
    let shallow = run(&["--depth", "2", &path]);
    assert!(shallow.status.success());
    let shallow = String::from_utf8(shallow.stdout).unwrap();
    let shallow = lines(&shallow);

    assert!(
        has(&shallow, "table [group, SpatialData table]"),
        "{shallow:?}"
    );
    assert!(has(&shallow, "observations: 167,780"), "{shallow:?}");
    assert!(has(&shallow, "variables: 313"), "{shallow:?}");
    assert!(has(&shallow, "X: csr [167780, 313]"), "{shallow:?}");
    assert!(has(&shallow, "obs columns: 3"), "{shallow:?}");
    assert!(has(&shallow, "var columns: 1"), "{shallow:?}");
    assert!(has(&shallow, "annotates: cell_circles"), "{shallow:?}");
    assert!(has(&shallow, "region key: region"), "{shallow:?}");
    assert!(has(&shallow, "instance key: cell_id"), "{shallow:?}");

    // At this depth the children of the table are not listed at all, which is
    // exactly the case the rows were written for.
    assert!(!has(&shallow, "obs [group]"), "{shallow:?}");

    // Deeper, and the subtree the rows summarise is all still there. The rows
    // did not replace it.
    let full = run(&[&path]);
    assert!(full.status.success());
    let full = String::from_utf8(full.stdout).unwrap();
    let full = lines(&full);

    for node in ["X [group]", "obs [group]", "var [group]", "_index [array]"] {
        assert!(
            has(&full, node),
            "{node} should still be walked into:\n{full:?}"
        );
    }

    // And the chunk under `X/data` is not a node, at any depth.
    assert!(!has(&full, "c [group]"), "{full:?}");
    assert!(!has(&full, "c [unknown]"), "{full:?}");

    fs::remove_dir_all(&dir).unwrap();
}

/// A small Zarr V3 SpatialData store whose declarations all hold.
///
/// One two-level image pyramid, and one table annotating it. Every rule
/// `--validate` knows has something to look at here and finds it: the OME
/// dataset paths exist, the levels agree with the axes, the table's region
/// names the image, and `X` is the shape the two indexes declare.
///
/// The broken store below is this one with three values changed, which is what
/// makes the pair worth reading side by side.
const SOUND: &[(&str, &str)] = &[
    (
        "zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "spatialdata_attrs": {"spatialdata_software_version": "0.4.0", "version": "0.1"}
        }}"#,
    ),
    (
        "images/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    ),
    (
        "images/morphology/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "spatialdata_attrs": {"version": "0.4"},
            "ome": {"version": "0.5", "multiscales": [{
                "axes": [{"name": "c"}, {"name": "y"}, {"name": "x"}],
                "datasets": [{"path": "0"}, {"path": "1"}]
            }]}
        }}"#,
    ),
    (
        "images/morphology/0/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [3, 64, 64],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [1, 32, 32]}},
            "data_type": "uint16"}"#,
    ),
    (
        "images/morphology/1/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [3, 32, 32],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [1, 32, 32]}},
            "data_type": "uint16"}"#,
    ),
    (
        "tables/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group"}"#,
    ),
    (
        "tables/table/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "encoding-type": "anndata", "encoding-version": "0.1.0",
            "spatialdata-encoding-type": "ngff:regions_table",
            "region": "morphology", "region_key": "region", "instance_key": "instance_id"
        }}"#,
    ),
    (
        "tables/table/obs/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "encoding-type": "dataframe", "_index": "_index", "column-order": ["region"]
        }}"#,
    ),
    (
        "tables/table/obs/_index/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [10],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [10]}},
            "data_type": "int64"}"#,
    ),
    (
        "tables/table/var/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "encoding-type": "dataframe", "_index": "_index", "column-order": []
        }}"#,
    ),
    (
        "tables/table/var/_index/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [4],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [4]}},
            "data_type": "int64"}"#,
    ),
    (
        "tables/table/X/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [10, 4],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [10, 4]}},
            "data_type": "float32"}"#,
    ),
];

/// The three files that make the sound store an unsound one, each breaking a
/// different rule: a multiscale declaring a level that was never written, a
/// table annotating an element that does not exist, and an `X` that is not the
/// shape its own indexes say it is.
const BROKEN: &[(&str, &str)] = &[
    (
        "images/morphology/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "spatialdata_attrs": {"version": "0.4"},
            "ome": {"version": "0.5", "multiscales": [{
                "axes": [{"name": "c"}, {"name": "y"}, {"name": "x"}],
                "datasets": [{"path": "0"}, {"path": "2"}]
            }]}
        }}"#,
    ),
    (
        "tables/table/zarr.json",
        r#"{"zarr_format": 3, "node_type": "group", "attributes": {
            "encoding-type": "anndata", "encoding-version": "0.1.0",
            "spatialdata-encoding-type": "ngff:regions_table",
            "region": "cells", "region_key": "region", "instance_key": "instance_id"
        }}"#,
    ),
    (
        "tables/table/X/zarr.json",
        r#"{"zarr_format": 3, "node_type": "array", "shape": [9, 4],
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [9, 4]}},
            "data_type": "float32"}"#,
    ),
];

#[test]
fn validation_passes_a_store_whose_declarations_all_hold() {
    let dir = fixture_dir("validate-sound");
    let root = dir.join("sound.zarr");
    write_store(&root, SOUND);

    let output = run(&["--validate", &root.to_string_lossy()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    fs::remove_dir_all(&dir).unwrap();

    // Nothing worse than a warning means the status a shell script tests for.
    assert_eq!(output.status.code(), Some(0), "{stdout}");

    for expected in [
        "PASS  /  Zarr root metadata is readable",
        "PASS  /images/morphology  OME dataset path \"0\" exists",
        "PASS  /images/morphology  OME dataset path \"1\" exists",
        "PASS  /images/morphology  pyramid levels agree with the multiscale's axes on 3 dimensions",
        "PASS  /tables/table  table region \"morphology\" names an existing SpatialData element",
        "PASS  /tables/table  AnnData X rows match the 10 observations the obs index declares",
        "PASS  /tables/table  AnnData X columns match the 4 variables the var index declares",
        "Validation: 12 passed, 0 warnings, 0 errors",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "expected {expected:?} in:\n{stdout}"
        );
    }

    // A sound store says so and nothing else.
    assert!(!stdout.contains("WARN"), "{stdout}");
    assert!(!stdout.contains("ERROR"), "{stdout}");
}

#[test]
fn validation_reports_a_declaration_the_store_does_not_have() {
    let dir = fixture_dir("validate-broken");
    let root = dir.join("broken.zarr");
    write_store(&root, SOUND);
    write_store(&root, BROKEN);

    let output = run(&["--validate", &root.to_string_lossy()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    fs::remove_dir_all(&dir).unwrap();

    // Two, and not one: a validation that found something is not a validation
    // that failed to run.
    assert_eq!(output.status.code(), Some(2), "{stdout}");

    for expected in [
        "ERROR /images/morphology  OME dataset path \"2\" does not exist",
        "ERROR /tables/table  table region \"cells\" does not name an existing SpatialData element",
        "ERROR /tables/table  AnnData X has 9 rows but the obs index declares 10 observations",
        "Validation: 9 passed, 0 warnings, 3 errors",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "expected {expected:?} in:\n{stdout}"
        );
    }

    // The rules that still hold still print. A store is not reported as
    // nothing but its faults.
    assert!(
        stdout.contains("PASS  /images/morphology  OME dataset path \"0\" exists"),
        "{stdout}"
    );
}

#[test]
fn validation_json_carries_the_same_findings_and_a_summary() {
    let dir = fixture_dir("validate-json");
    let root = dir.join("broken.zarr");
    write_store(&root, SOUND);
    write_store(&root, BROKEN);

    let output = run(&["--validate", "--json", &root.to_string_lossy()]);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(output.status.code(), Some(2), "{stdout}");

    let report: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("--validate --json should print valid JSON: {error}"));

    assert_eq!(
        report["summary"],
        json!({"passed": 9, "warnings": 0, "errors": 3})
    );

    let findings = report["findings"].as_array().expect("a list of findings");
    assert_eq!(findings.len(), 12, "{stdout}");
    // The first finding is the first line of the report, so the two outputs
    // are the same findings in the same order.
    assert_eq!(
        findings[0],
        json!({
            "severity": "pass",
            "path": "/",
            "message": "Zarr root metadata is readable",
        })
    );
    assert!(
        findings.iter().any(|found| {
            found["severity"] == "error"
                && found["path"] == "/tables/table"
                && found["message"]
                    == "table region \"cells\" does not name an existing SpatialData element"
        }),
        "{stdout}"
    );
}
