use std::env;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use serde_json::{Value, json};

/// What kind of Zarr node a directory is, as far as its metadata files reveal.
enum NodeKind {
    /// A Zarr group, together with whatever its attributes say about it.
    Group(GroupMeta),
    Array(ArrayMeta),
    Unknown,
}

/// What a group's attributes say about it, beyond its being a group.
///
/// The two fields are independent facts, read by two conventions that know
/// nothing about each other. A group may carry either, both, or -- as almost
/// every group does -- neither. A SpatialData raster element carries both, and
/// each field is still read on its own terms.
struct GroupMeta {
    /// Set when the group carries OME-Zarr image metadata. See `ome_info`.
    ome: Option<OmeInfo>,
    /// What this group is in SpatialData's own vocabulary, when it says
    /// anything at all. `None` for every ordinary Zarr group, and for the
    /// `images`/`labels`/`points`/`shapes`/`tables` containers inside a store,
    /// which carry no attributes. See `spatialdata_info`.
    spatialdata: Option<SpatialData>,
}

/// What a group says about itself in [SpatialData]'s own vocabulary.
///
/// A group is either the root of a store or one element inside one, and never
/// both. One field of this type says that better than an `Option` per kind
/// would: most of the combinations two or three of those could represent
/// cannot occur.
///
/// Which one it is comes from three unrelated markers -- see
/// `spatialdata_info` -- but the answer is always a single value.
///
/// [SpatialData]: https://spatialdata.scverse.org/
enum SpatialData {
    /// The root of a store, holding the container format version when one was
    /// recorded. See `spatialdata_root`.
    Root(Option<String>),
    /// A points element: transcript locations, molecule detections. See
    /// `spatialdata_encoded_element`.
    Points,
    /// A shapes element: cell boundaries, circles, landmarks.
    Shapes,
    /// A table element: an AnnData annotation table over the regions of
    /// another element.
    Table,
    /// A raster image element: microscopy, morphology. See
    /// `spatialdata_raster_element`.
    Image,
    /// A raster segmentation element: one integer label per pixel.
    Labels,
}

impl SpatialData {
    /// The text printed inside a group's label.
    ///
    /// A root is tagged with the *container* format version. An element is
    /// tagged with its kind and no version at all: the number an element
    /// records is the version of its own encoding, a different quantity that
    /// would collide confusingly with the container version shown on the root
    /// line -- in a container 0.2 store the points element is 0.2 and the
    /// shapes element is 0.3.
    fn tag(&self) -> String {
        match self {
            SpatialData::Root(Some(version)) => format!("SpatialData {version}"),
            SpatialData::Root(None) => String::from("SpatialData"),
            SpatialData::Points => String::from("SpatialData points"),
            SpatialData::Shapes => String::from("SpatialData shapes"),
            SpatialData::Table => String::from("SpatialData table"),
            SpatialData::Image => String::from("SpatialData image"),
            SpatialData::Labels => String::from("SpatialData labels"),
        }
    }

    /// The kind on its own, without the convention in front of it, for output
    /// that has already said which convention it is talking about.
    fn kind(&self) -> &'static str {
        match self {
            SpatialData::Root(_) => "root",
            SpatialData::Points => "points",
            SpatialData::Shapes => "shapes",
            SpatialData::Table => "table",
            SpatialData::Image => "image",
            SpatialData::Labels => "labels",
        }
    }

    /// The container format version, which only a store root records.
    fn version(&self) -> Option<&str> {
        match self {
            SpatialData::Root(version) => version.as_deref(),
            _ => None,
        }
    }
}

/// The three fields shown underneath an array. Each is optional on its own, so
/// metadata missing `chunks` still shows the shape it does have.
///
/// The two dimension lists are kept as the values the file held rather than as
/// finished text, because two renderers now draw on them and they want
/// different things: the tree wants `[4096, 4096]`, and `--json` wants a real
/// JSON array. Formatting once, here, would have forced the second to unpick
/// the first.
///
/// Nothing interprets the entries. A dimension is copied across whatever it
/// was written as, so a malformed `"shape": [1, "x"]` survives to the output
/// instead of being dropped on the way.
///
/// `shards` is the one field that is absent rather than unread when it is
/// `None`: only a V3 array using the sharding codec has shards at all, so
/// every other array simply has nothing to say here.
struct ArrayMeta {
    shape: Option<Vec<Value>>,
    chunks: Option<Vec<Value>>,
    shards: Option<Vec<Value>>,
    dtype: Option<String>,
}

/// What an OME-Zarr image group carries.
///
/// The `Option` sits outside this struct rather than around its fields: a group
/// either is an OME-Zarr image, in which case it always has at least a version
/// slot, or it is an ordinary group and there is nothing here at all.
struct OmeInfo {
    /// The metadata version, exactly as stored. `None` when the file records
    /// none, which real 0.4 stores often do not.
    version: Option<String>,
    /// The axis names, in the order the metadata lists them. `None` when the
    /// metadata carries no axes (OME-NGFF 0.1 and 0.2) or we could not read
    /// them.
    axes: Option<Vec<String>>,
    /// The declared resolution levels, as the paths the metadata lists, in the
    /// order it lists them. `None` when there is no usable `datasets` array.
    ///
    /// Held as a list rather than as a finished row because two rows are drawn
    /// from it -- the level count and the paths themselves -- and they are one
    /// fact shown twice. Keeping the list is what stops the two from drifting:
    /// the count is simply its length.
    datasets: Option<Vec<String>>,
}

impl OmeInfo {
    /// The label tag: "OME-Zarr 0.5", or "OME-Zarr" when no version was read.
    ///
    /// Built here rather than stored, so that the version and the text made
    /// from it cannot drift apart -- the same reason `pyramid levels` is the
    /// length of `datasets` rather than a number of its own.
    fn tag(&self) -> String {
        match &self.version {
            Some(version) => format!("OME-Zarr {version}"),
            None => String::from("OME-Zarr"),
        }
    }
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
    zarr-tree [OPTIONS] <DIRECTORY>

OPTIONS:
        --depth <N>  Descend at most N levels below the root.
                     0 shows the root on its own. Omitted, the whole store is
                     walked. Arrays are leaves at any depth.
        --json       Print the same tree as JSON, one object per node.
                     Combines with --depth.
    -h, --help       Print help
    -V, --version    Print version";

/// The one-line reminder printed on stderr when the command line does not make
/// sense. Kept in step with the USAGE section above by hand: there are two of
/// them because one is an answer and the other is a complaint.
const USAGE: &str = "usage: zarr-tree [OPTIONS] <DIRECTORY>";

/// What the command line asked for.
struct Options {
    /// The directory to walk, exactly as it was typed.
    path: String,
    /// How many levels below the root to descend, or `None` for all of them.
    /// `Some(0)` shows the root and nothing under it.
    depth: Option<usize>,
    /// Print JSON instead of the tree. The two show the same facts about the
    /// same nodes; only the shape of the output differs.
    json: bool,
}

/// What `parse_args` made of the command line.
///
/// The two flags are answered without touching the filesystem, so they are
/// variants of their own rather than fields nothing else would look at.
enum Request {
    Walk(Options),
    Help,
    Version,
}

impl NodeKind {
    /// The tag printed after a directory name.
    ///
    /// A group carrying OME-Zarr image metadata says so here, as
    /// `[group, OME-Zarr 0.4]`. That text is built at run time from the version
    /// found in the file, so this can no longer hand back a `&'static str`
    /// borrowing a literal compiled into the binary: it returns an owned
    /// `String` instead.
    ///
    /// A group can say more than one thing about itself, so its tags are
    /// collected into a list and joined. That is one arm for every group
    /// rather than one arm per combination, and it keeps the two conventions
    /// independent: OME-Zarr comes first only because it is pushed first, not
    /// because it outranks anything.
    fn label(&self) -> String {
        match self {
            NodeKind::Group(meta) => {
                let mut tags = vec![String::from("group")];
                // `clone` copies a short string we are about to print. This
                // function hands back an owned String either way, so there is
                // nothing to be gained by borrowing here.
                if let Some(ome) = &meta.ome {
                    tags.push(ome.tag());
                }
                if let Some(spatialdata) = &meta.spatialdata {
                    tags.push(spatialdata.tag());
                }
                format!("[{}]", tags.join(", "))
            }
            NodeKind::Array(_) => String::from("[array]"),
            NodeKind::Unknown => String::from("[unknown]"),
        }
    }

    /// The Zarr node kind on its own, without the brackets the tree draws
    /// around it or the tags it collects inside them.
    fn kind(&self) -> &'static str {
        match self {
            NodeKind::Group(_) => "group",
            NodeKind::Array(_) => "array",
            NodeKind::Unknown => "unknown",
        }
    }
}

fn main() {
    // One error path for everything `run` does, and one special case in it.
    //
    // `zarr-tree store.zarr | head` closes the pipe as soon as `head` has the
    // lines it wanted, and every write after that fails with `BrokenPipe`.
    // That is not a failure of ours: the reader said it had seen enough. The
    // Unix convention for a program at the producing end of a pipeline is to
    // stop there, quietly and successfully, which is what a plain `return`
    // does -- no message on stderr, exit status 0.
    //
    // Every other error keeps exactly the behaviour it had: a line on stderr
    // and exit status 1. A directory we cannot read is still an error worth
    // reporting; `BrokenPipe` cannot reach us from the filesystem.
    if let Err(error) = run() {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return;
        }
        eprintln!("error: {error}");
        process::exit(1);
    }
}

/// Everything `main` used to do, with the writes routed through one handle so
/// that a failed one comes back as a value rather than a panic.
///
/// `println!` writes to stdout and panics if that write fails, which is how a
/// closed pipe used to end this program: a panic message on stderr and exit
/// status 101. Writing through a handle of our own turns the same failure into
/// an ordinary `io::Error` that `?` carries up to `main`.
///
/// The handle is locked once here rather than per line -- `println!` takes the
/// same lock on every call -- and passed down the walk. `&mut dyn Write` is a
/// trait object: the functions below neither know nor care that this is
/// standard output.
fn run() -> io::Result<()> {
    // args[0] is the program itself, so the command line proper starts at 1.
    let args: Vec<String> = env::args().collect();

    // The argument errors exit directly. They are settled before anything is
    // written, they belong on stderr rather than in the tree, and routing them
    // through the `io::Result` would mean inventing an io::Error for something
    // that is not an I/O failure at all.
    let request = match parse_args(&args[1..]) {
        Ok(request) => request,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!("{USAGE}");
            process::exit(1);
        }
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let options = match request {
        Request::Help => {
            writeln!(out, "{HELP}")?;
            return out.flush();
        }
        Request::Version => {
            // env! reads the variable when the crate is compiled, so this is a
            // plain string literal in the binary. Cargo fills it in from the
            // version field in Cargo.toml, which is why the two cannot drift
            // apart.
            writeln!(out, "zarr-tree {}", env!("CARGO_PKG_VERSION"))?;
            return out.flush();
        }
        Request::Walk(options) => options,
    };

    let root = Path::new(&options.path);
    if !root.exists() {
        eprintln!("error: path does not exist: {}", root.display());
        process::exit(1);
    }
    if !root.is_dir() {
        eprintln!("error: path is not a directory: {}", root.display());
        process::exit(1);
    }

    // The root is named by the path as it was typed, in both outputs. Every
    // node below it is named by its directory.
    let root_name = options.path.trim_end_matches('/');

    if options.json {
        let tree = json_tree(root_name, root, options.depth)?;
        // Indented rather than compact: this is still a command a person runs
        // and reads, and `jq` does not mind either way.
        writeln!(out, "{}", serde_json::to_string_pretty(&tree)?)?;
        return out.flush();
    }

    let root_kind = classify(root);
    writeln!(out, "{root_name} {}", root_kind.label())?;

    // An array is a leaf here too: its metadata takes the place of the walk.
    match &root_kind {
        NodeKind::Array(meta) => print_array_meta(&mut out, meta, "")?,
        _ => print_tree(&mut out, root, "", &group_rows(&root_kind), options.depth)?,
    }

    // Stdout flushes itself when the process ends, but it swallows any error
    // in doing so. Flushing here is what puts a late `BrokenPipe` in front of
    // the handler above instead of losing it.
    out.flush()
}

/// Read the command line, or say what is wrong with it.
///
/// `args` is everything after the program name. Hand-written rather than
/// reached for a crate: three options and one directory is a short enough
/// grammar that a loop over the arguments says all there is to say, and the
/// package still has one dependency.
///
/// The loop holds its iterator rather than using a `for`, because `--depth`
/// has to reach forward and take the value that follows it.
fn parse_args(args: &[String]) -> Result<Request, String> {
    let mut path: Option<&str> = None;
    let mut depth: Option<usize> = None;
    let mut json = false;

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // Answered on sight, so neither reaches the checks below and a
            // `--help` anywhere on the line still prints the help.
            "-h" | "--help" => return Ok(Request::Help),
            "-V" | "--version" => return Ok(Request::Version),
            "--depth" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("--depth needs a number, as in --depth 2"))?;
                // `usize` does the validating: a negative number and anything
                // that is not a number at all both fail to parse, and the
                // message quotes what was actually typed.
                depth = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--depth needs a whole number, not {value:?}"))?,
                );
            }
            // Repeating it is not an error: it asks for the same thing twice.
            "--json" => json = true,
            // A leading dash we do not know is a mistyped option rather than a
            // directory. The cost of reading it that way is that a directory
            // whose name begins with `-` can no longer be inspected -- the
            // same trade `-h` and `-V` already made.
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other => {
                if path.is_some() {
                    return Err(String::from("expected exactly one directory"));
                }
                path = Some(other);
            }
        }
    }

    let path = path.ok_or_else(|| String::from("expected a directory"))?;
    Ok(Request::Walk(Options {
        path: String::from(path),
        depth,
        json,
    }))
}

/// Print the directories inside `dir`, one line each, indented by `prefix`.
///
/// `rows` is `dir`'s own metadata, one finished line each, in the order they
/// should appear. They are printed here rather than by the caller because this
/// is the one place that already knows whether any children follow them, which
/// is what decides the last connector. An empty slice means there is nothing to
/// print above the children.
///
/// `out` is where the lines go. It was already returning `io::Result` for the
/// directory reads; writes now join them, so a closed pipe stops the walk the
/// same way an unreadable directory does.
///
/// `depth` is how many more levels below `dir` may be listed, or `None` for
/// all of them. `Some(0)` means this node is as deep as the output goes: its
/// own metadata rows still print, because those describe the node itself
/// rather than anything below it.
fn print_tree(
    out: &mut dyn Write,
    dir: &Path,
    prefix: &str,
    rows: &[String],
    depth: Option<usize>,
) -> io::Result<()> {
    let subdirs = child_dirs(dir, depth)?;

    // This directory's own metadata, above its children. Metadata rows keep
    // the shorter two-dash stem that tells them apart from node rows.
    for (i, row) in rows.iter().enumerate() {
        // `└─` closes a branch, so it belongs to the last row only when there
        // are no children below it to keep the branch open.
        let is_last = i == rows.len() - 1 && subdirs.is_empty();
        let connector = if is_last { "└─ " } else { "├─ " };
        writeln!(out, "{prefix}{connector}{row}")?;
    }

    for (i, path) in subdirs.iter().enumerate() {
        let is_last = i == subdirs.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let name = node_name(path);
        let kind = classify(path);
        writeln!(out, "{prefix}{connector}{name} {}", kind.label())?;

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
            // Arrays are leaves at any depth: what lies beneath them is chunk
            // storage, so the limit has nothing to say here. Their metadata
            // rows describe the array itself and print as they always did.
            NodeKind::Array(meta) => print_array_meta(out, meta, &child_prefix)?,
            // One level spent. `map` leaves `None` as `None`, which is how an
            // unlimited walk stays unlimited without a second branch.
            _ => print_tree(
                out,
                path,
                &child_prefix,
                &group_rows(&kind),
                depth.map(|depth| depth - 1),
            )?,
        }
    }

    Ok(())
}

/// The subdirectories of `dir`, sorted, and honouring the depth limit.
///
/// Collected rather than streamed because the tree needs to know which child is
/// last before it can draw a connector, and `read_dir` returns entries in
/// arbitrary order.
///
/// `Some(0)` means this node is as deep as the output goes, and the directory
/// is then not read at all. That is not only cheaper on a store full of chunk
/// files: an empty list is also exactly what a node with no children has, so
/// neither renderer needs a rule of its own for the limit.
///
/// Both renderers call this, which is what keeps them agreeing about which
/// children exist and in what order.
fn child_dirs(dir: &Path, depth: Option<usize>) -> io::Result<Vec<PathBuf>> {
    if depth == Some(0) {
        return Ok(Vec::new());
    }

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

    Ok(subdirs)
}

/// The name a node is listed under: its directory name.
///
/// A path with no final component -- which the walk never produces, since
/// every entry comes from `read_dir` -- yields an empty name rather than
/// failing. A name that is not valid UTF-8 keeps its readable parts, with the
/// rest replaced, which is what `to_string_lossy` is for.
fn node_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
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
        // Each helper opens `.zattrs` for itself, so a V2 group costs one
        // extra read of a small file. That buys each fact being read where it
        // is explained; the V3 path below parses its one file once and hands
        // the same value to both.
        return NodeKind::Group(GroupMeta {
            ome: ome_info_v2(dir),
            spatialdata: spatialdata_info_v2(dir),
        });
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
        "group" => Some(NodeKind::Group(GroupMeta {
            ome: ome_info_v3(&value),
            spatialdata: spatialdata_info_v3(&value),
        })),
        "array" => Some(NodeKind::Array(array_meta_v3(&value))),
        _ => None,
    }
}

/// Look for OME-Zarr image metadata beside a Zarr V2 `.zgroup`.
///
/// V2 keeps user attributes in a `.zattrs` file separate from `.zgroup`, so
/// this is the one extra read the tag costs us. A `.zattrs` that is missing or
/// unparseable simply means no tag.
fn ome_info_v2(dir: &Path) -> Option<OmeInfo> {
    let attrs = read_json(&dir.join(".zattrs"))?;

    // V2 predates the `ome` namespace: the keys sit at the top level of
    // `.zattrs`, and the version belongs to an individual multiscale rather
    // than to the group. Real 0.4 stores often leave it out entirely.
    let version = attrs
        .get("multiscales")
        .and_then(|value| value.as_array())
        .and_then(|entries| entries.first())
        .and_then(|first| first.get("version"));

    ome_info(&attrs, version)
}

/// Look for OME-Zarr image metadata in an already-parsed Zarr V3 `zarr.json`.
///
/// V3 keeps group attributes in that same file, so nothing extra is read here.
/// The OME-Zarr keys live under a namespace of their own, version included.
fn ome_info_v3(value: &Value) -> Option<OmeInfo> {
    let ome = value.get("attributes")?.get("ome")?;
    ome_info(ome, ome.get("version"))
}

/// Collect what we display about an OME-Zarr image group, or `None` if this is
/// an ordinary Zarr group.
///
/// `ome` is whichever object holds the OME-Zarr keys -- the whole `.zattrs` for
/// V2, `attributes.ome` for V3 -- and `version` is passed in separately because
/// that is the only other thing the two layouts disagree about.
fn ome_info(ome: &Value, version: Option<&Value>) -> Option<OmeInfo> {
    // A `multiscales` key holding a non-empty array is what makes a group an
    // image. Missing, the wrong JSON type, or empty all mean it is not one:
    // `?` handles the first two without any error handling of ours, and
    // `first()` the third.
    let first = ome.get("multiscales")?.as_array()?.first()?;

    // Kept exactly as stored, and never checked against the versions we happen
    // to know about: this tool reports metadata rather than validating it. A
    // version that is absent, or is not a string, just leaves the tag bare.
    let version = version.and_then(|value| value.as_str()).map(String::from);

    // Axes belong to an individual multiscale rather than to the group, so they
    // come from that same first entry in both layouts -- the one part of this
    // metadata V2 and V3 already agree about.
    //
    // `datasets` is the one part of this metadata that has not changed shape
    // since OME-NGFF 0.1 -- always a list of objects with a `path` -- so unlike
    // the axes it needs no per-version handling at all. What 0.4 added to each
    // entry, `coordinateTransformations`, is not read.
    Some(OmeInfo {
        version,
        axes: axis_names(first.get("axes")),
        datasets: dataset_paths(first.get("datasets")),
    })
}

/// Look for SpatialData metadata beside a Zarr V2 `.zgroup`.
///
/// V2 keeps user attributes in a `.zattrs` file, and that file *is* the
/// attributes object -- so the markers sit at its top level, rather than under
/// an `attributes` field the way V3 nests it. A `.zattrs` that is missing or
/// unparseable simply means no tag.
///
/// This opens `.zattrs` a second time, after `ome_info_v2` has already read it.
/// The file is small, V2 is the older of the two SpatialData layouts, and
/// reading each fact where that fact is explained costs less confusion than it
/// costs microseconds.
fn spatialdata_info_v2(dir: &Path) -> Option<SpatialData> {
    let attrs = read_json(&dir.join(".zattrs"))?;
    // V2 predates the `ome` namespace, so the OME-Zarr keys sit at the top
    // level of `.zattrs` alongside SpatialData's own -- the two objects this
    // function has to tell apart are, here, one and the same.
    spatialdata_info(&attrs, Some(&attrs))
}

/// Look for SpatialData metadata in an already-parsed Zarr V3 `zarr.json`.
///
/// V3 keeps group attributes in that same file, so nothing extra is read here.
fn spatialdata_info_v3(value: &Value) -> Option<SpatialData> {
    let attrs = value.get("attributes")?;
    // V3 keeps the OME-Zarr keys in a namespace of their own. A group with no
    // `ome` key is simply not a raster, which `spatialdata_raster_element`
    // handles as the `None` it is handed.
    spatialdata_info(attrs, attrs.get("ome"))
}

/// What a group says about itself in SpatialData's vocabulary, or `None` when
/// it says nothing.
///
/// `attrs` is whichever object holds the group's attributes -- the whole
/// `.zattrs` for V2, `attributes` for V3 -- and `ome` is whichever object
/// inside it holds the OME-Zarr keys, which is `attrs` itself in V2 and
/// `attrs.ome` in V3. That is the same split `ome_info` handles, and it is
/// passed in separately for the same reason: it is the one thing the two
/// layouts disagree about.
///
/// [SpatialData] keeps a spatial omics experiment in a Zarr container: images,
/// segmentation masks, transcript locations, geometries and annotation tables,
/// all in one store.
///
/// Three independent markers are read, in three different keys, and a group is
/// only ever one of them. They are tried in order, so a group that somehow
/// carried more than one would be reported as the first that matched -- the
/// store root before anything inside a store.
///
/// Nothing here looks at directory names. In a real store the `images`,
/// `points`, `shapes` and `tables` groups carry no attributes at all, so a
/// name would be the only thing left to go on -- and an ordinary Zarr store
/// whose children happen to be called `points` and `shapes` is not a
/// SpatialData store.
///
/// [SpatialData]: https://spatialdata.scverse.org/
fn spatialdata_info(attrs: &Value, ome: Option<&Value>) -> Option<SpatialData> {
    spatialdata_root(attrs)
        .or_else(|| spatialdata_encoded_element(attrs))
        .or_else(|| spatialdata_raster_element(attrs, ome))
}

/// The root of a SpatialData store, or `None` when this group is not one.
///
/// The elements *inside* a store carry a `spatialdata_attrs` of their own,
/// holding just the version of their own encoding -- so the presence of that
/// object proves nothing. Only the root records the software that wrote it,
/// which is why `spatialdata_software_version` is what is required. Without
/// that check, every image, label, point and shape group would be reported as
/// a store of its own.
///
/// A store written before that version was recorded carries no marker at all,
/// and is left untagged rather than guessed at. Its *elements* are still
/// recognised, as far as they name themselves: `spatialdata_encoded_element`
/// reads a different key, which those older stores do carry.
fn spatialdata_root(attrs: &Value) -> Option<SpatialData> {
    let spatialdata_attrs = attrs.get("spatialdata_attrs")?;

    // The discriminator, read for its presence rather than its value. `get` on
    // anything that is not an object yields None, so a `spatialdata_attrs`
    // that is a string or a number falls out here too, with no type check of
    // ours.
    spatialdata_attrs.get("spatialdata_software_version")?;

    // Shown exactly as stored, and never checked against the versions we
    // happen to know about: this tool reports metadata rather than validating
    // it. A version that is absent, or is not a string, just leaves the tag
    // bare -- the same way an OME-Zarr image with no readable version does.
    let version = spatialdata_attrs
        .get("version")
        .and_then(|value| value.as_str())
        .map(String::from);

    Some(SpatialData::Root(version))
}

/// An element that names its own kind, or `None` when this group is not one.
///
/// SpatialData writes the kind of its non-raster elements into the attributes
/// as a plain string. The values read here have been written unchanged by
/// every release of the library, and are the same in Zarr V2 and V3, so
/// recognition needs no per-version handling: what changed between format
/// versions is where the *payload* lives, and no payload is read.
///
/// Two key names carry it, for a historical reason rather than a semantic
/// one. Points and shapes use `encoding-type`; a table's group is written by
/// AnnData, which claims that key for its own `"anndata"`, so SpatialData
/// records the kind one key over. One list of values is tried under both
/// names: they are drawn from a single namespace, no store writes them
/// crosswise, and a kind added later needs adding in one place.
///
/// Each value is matched exactly, never by prefix or by the mere presence of
/// a key. AnnData writes `encoding-type` throughout the subtree beneath a
/// table -- `"dataframe"`, `"csr_matrix"`, `"array"` -- and none of those is a
/// SpatialData element.
///
/// An element carries no version in its tag, so nothing is read here beyond
/// this one string. What it does carry -- axis names, a feature key, an
/// instance key, the region a table annotates -- is scientific metadata this
/// version does not display.
fn spatialdata_encoded_element(attrs: &Value) -> Option<SpatialData> {
    encoded_kind(attrs, "encoding-type")
        .or_else(|| encoded_kind(attrs, "spatialdata-encoding-type"))
}

/// The element kind named by `key`, or `None` when it names nothing we know.
fn encoded_kind(attrs: &Value, key: &str) -> Option<SpatialData> {
    match attrs.get(key)?.as_str()? {
        "ngff:points" => Some(SpatialData::Points),
        "ngff:shapes" => Some(SpatialData::Shapes),
        "ngff:regions_table" => Some(SpatialData::Table),
        _ => None,
    }
}

/// A raster element inside a SpatialData store, or `None` when this group is
/// not one.
///
/// Rasters name themselves nowhere: SpatialData writes them through the
/// OME-Zarr writers, which have no `encoding-type` of their own, so both
/// halves of this answer come from somewhere else.
///
/// *That* it is a SpatialData element comes from `spatialdata_attrs`. Every
/// raster SpatialData writes gets one, holding the version of its own
/// encoding. On its own that object is weak evidence -- `spatialdata_root`
/// explains why it is not enough to prove a store root -- but paired with
/// OME-Zarr image metadata it is what separates an element of a store from an
/// ordinary OME-Zarr image that has nothing to do with SpatialData. Without
/// it, every microscopy image ever written would be reported as a SpatialData
/// element.
///
/// *Which* raster it is comes from OME-Zarr. A segmentation is a multiscale
/// image like any other, distinguished only by an `image-label` object beside
/// its `multiscales`, which describes the colours and properties of the label
/// values. Read here for its presence alone: nothing inside it is displayed,
/// and no label value is ever looked at.
///
/// The specification says a label image SHOULD carry that key, not MUST, so a
/// segmentation that omits it is reported as an image. Reporting what the
/// metadata declares is this tool's rule, and the alternative would be to
/// guess from the `labels/` directory name.
fn spatialdata_raster_element(attrs: &Value, ome: Option<&Value>) -> Option<SpatialData> {
    // SpatialData's mark on an element it wrote. Required to be an object, so
    // that a `spatialdata_attrs` holding a string or a number proves nothing.
    attrs.get("spatialdata_attrs")?.as_object()?;

    // And the OME-Zarr metadata that makes it a raster, tested exactly as
    // `ome_info` tests it: a `multiscales` key holding a non-empty array.
    let ome = ome?;
    ome.get("multiscales")?.as_array()?.first()?;

    match ome.get("image-label") {
        Some(_) => Some(SpatialData::Labels),
        None => Some(SpatialData::Image),
    }
}

/// The metadata rows to print underneath a node's own line, in display order.
///
/// Everything but an OME-Zarr image group has nothing to say here and gets an
/// empty list. Returning one list rather than one argument per row keeps
/// `print_tree` from growing a parameter every time a row is added: all it
/// needs to know is how many rows there are and what each one says.
fn group_rows(kind: &NodeKind) -> Vec<String> {
    // `let ... else` matches one pattern and takes an early exit when it does
    // not fit, which reads better here than a `match` whose other arm is just
    // an empty list.
    // The `..` says the rest of GroupMeta is not needed here: no SpatialData
    // marker adds a row below its own line in this version.
    let NodeKind::Group(GroupMeta { ome: Some(ome), .. }) = kind else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    if let Some(axes) = &ome.axes {
        rows.push(format!("axes: {}", axes.join(", ")));
    }
    // Both rows come from the same list, so they appear and vanish together.
    if let Some(datasets) = &ome.datasets {
        rows.push(format!("pyramid levels: {}", datasets.len()));
        rows.push(format!("datasets: {}", datasets.join(", ")));
    }
    rows
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
            shards: None,
            dtype: None,
        };
    };

    ArrayMeta {
        shape: dims(value.get("shape")),
        chunks: dims(value.get("chunks")),
        // V2 has no sharding: there is one grid and `chunks` is it.
        shards: None,
        // Shown exactly as stored, in V2's NumPy notation ("<u2", "|u1").
        dtype: value
            .get("dtype")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Collect the display metadata from an already-parsed Zarr V3 `zarr.json`.
fn array_meta_v3(value: &Value) -> ArrayMeta {
    // V3 keeps the grid shape inside the chunk grid description. Only the
    // "regular" grid has a `chunk_shape`; any other grid simply yields None
    // here and the row shows as missing.
    let grid_shape = value
        .get("chunk_grid")
        .and_then(|grid| grid.get("configuration"))
        .and_then(|config| config.get("chunk_shape"));

    // Under the sharding codec the grid no longer describes the chunks: it
    // describes the shards, and the chunks live inside them. The codec is
    // found by its exact name, never guessed at from the shapes themselves.
    let sharding = value
        .get("codecs")
        .and_then(|codecs| codecs.as_array())
        .and_then(|codecs| {
            codecs.iter().find(|codec| {
                codec.get("name").and_then(|name| name.as_str()) == Some("sharding_indexed")
            })
        });

    // Which shape is which is decided by whether the codec is *there*, not by
    // whether its inner shape could be read. A sharding codec we cannot read
    // through leaves `chunks` missing -- the `?` any unreadable field gets --
    // rather than quietly falling back to the shard shape under the wrong
    // name, which is the very mistake this branch exists to avoid.
    let (chunks, shards) = match sharding {
        Some(codec) => (
            dims(
                codec
                    .get("configuration")
                    .and_then(|config| config.get("chunk_shape")),
            ),
            dims(grid_shape),
        ),
        None => (dims(grid_shape), None),
    };

    ArrayMeta {
        shape: dims(value.get("shape")),
        chunks,
        shards,
        // The object form used by dtype extensions is not interpreted.
        dtype: value
            .get("data_type")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Read a dimension list -- a `shape`, a `chunks`, a `chunk_shape` -- as the
/// values it holds, or `None` if it is missing or is not an array.
///
/// The entries are copied rather than interpreted: whatever the file said a
/// dimension was, that is what comes out.
fn dims(value: Option<&Value>) -> Option<Vec<Value>> {
    Some(value?.as_array()?.clone())
}

/// Render a dimension list as `[4096, 4096]`.
///
/// `Value::to_string` gives each entry its JSON spelling, so a number prints
/// as a number and anything else prints as whatever it is.
fn format_dims(dims: &[Value]) -> String {
    let items: Vec<String> = dims.iter().map(|item| item.to_string()).collect();
    format!("[{}]", items.join(", "))
}

/// Read a `multiscales` entry's `axes` as the list of names it declares, or
/// `None` when there is nothing to show.
///
/// Both spellings are handled in one pass: OME-NGFF 0.3 stores each axis as a
/// bare dimension name, 0.4 and 0.5 as an object with a `name`. Only the name
/// is read -- `type` and `unit` are not displayed.
///
/// An entry we cannot read a name from becomes `?` rather than being dropped,
/// so the number of axes shown always matches the number the file declares.
fn axis_names(value: Option<&Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    if items.is_empty() {
        return None;
    }

    let names: Vec<String> = items
        .iter()
        .map(|item| {
            // Try the 0.3 form, then the 0.4/0.5 form, then give up on this
            // one entry alone.
            let name = item
                .as_str()
                .or_else(|| item.get("name").and_then(|name| name.as_str()))
                .unwrap_or("?");
            String::from(name)
        })
        .collect();

    Some(names)
}

/// Read a `multiscales` entry's `datasets` as the list of paths it declares,
/// or `None` when there is nothing to show.
///
/// The paths are shown exactly as stored. `"0"`, `"1"`, `"2"` is only a
/// convention -- `"s0"`, `"full"` and nested paths such as `"a/b"` are all
/// legal -- so nothing here sorts, renumbers or interprets them.
///
/// An entry we cannot read a path from becomes `?` rather than being dropped,
/// the same way `axis_names` treats a nameless axis: dropping it would report
/// a three-level pyramid as a two-level one.
fn dataset_paths(value: Option<&Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    if items.is_empty() {
        return None;
    }

    let paths: Vec<String> = items
        .iter()
        .map(|item| {
            // `get` on anything that is not an object yields None, so an entry
            // that is a bare string or a number lands on `?` too.
            let path = item
                .get("path")
                .and_then(|path| path.as_str())
                .unwrap_or("?");
            String::from(path)
        })
        .collect();

    Some(paths)
}

/// Print the metadata rows that sit underneath an array line.
///
/// `shape`, `chunks` and `dtype` are always printed, in that order. A sharded
/// array gains a `shards` row between `chunks` and `dtype`; every other array
/// prints exactly the three rows it always has. Whichever row ends up last
/// carries the closing connector. A field we could not read shows as `?`.
fn print_array_meta(out: &mut dyn Write, meta: &ArrayMeta, prefix: &str) -> io::Result<()> {
    // The dimension lists are drawn here rather than kept ready-made, because
    // `--json` wants the same facts as JSON arrays. Rendering late is what
    // lets one reading serve both.
    let shape = meta.shape.as_deref().map(format_dims);
    let chunks = meta.chunks.as_deref().map(format_dims);
    let shards = meta.shards.as_deref().map(format_dims);

    // A `Vec` rather than the fixed array this used to be, because the number
    // of rows is no longer fixed. Everything else about the drawing is the
    // same, including which name the padding is sized to -- "shards:" is the
    // same width as "chunks:".
    let mut rows = vec![("shape:", shape.as_deref()), ("chunks:", chunks.as_deref())];

    if let Some(shards) = shards.as_deref() {
        rows.push(("shards:", Some(shards)));
    }

    rows.push(("dtype:", meta.dtype.as_deref()));

    // Taken before the rows are consumed below, so the closing connector lands
    // on whichever row turned out to be last.
    let last = rows.len() - 1;

    for (i, (name, value)) in rows.into_iter().enumerate() {
        let connector = if i == last { "└─ " } else { "├─ " };
        let value = value.unwrap_or("?");
        // Pad to the width of the longest name, "chunks:", so values line up.
        writeln!(out, "{prefix}{connector}{name:<7} {value}")?;
    }

    Ok(())
}

/// The whole tree under `dir` as one JSON value, ready to be printed.
///
/// This walks the same directories as `print_tree` and asks `classify` the
/// same questions -- it is a second *renderer*, not a second reading of the
/// metadata. Every fact below comes out of the same `NodeKind` the tree
/// prints, so the two outputs cannot disagree about what a store contains.
///
/// The walk is duplicated rather than shared through a tree of nodes built
/// once, because building one would cost the tree its streaming: today
/// `zarr-tree big.zarr | head` prints its first lines immediately and stops as
/// soon as the reader does. JSON has no such option -- a document is a whole
/// or it is nothing -- so only this side pays for being assembled in memory.
///
/// Each node carries its `name`, its Zarr `kind`, and its `children`. The
/// metadata sections -- `array`, `ome`, `spatialdata` -- appear only on the
/// nodes they apply to, and a field inside one is `null` when the file did not
/// give us a readable value. That is the same rule the tree follows when it
/// prints `?`.
fn json_tree(name: &str, dir: &Path, depth: Option<usize>) -> io::Result<Value> {
    let kind = classify(dir);

    let mut node = json!({
        "name": name,
        "kind": kind.kind(),
    });

    match &kind {
        NodeKind::Array(meta) => node["array"] = json_array_meta(meta),
        NodeKind::Group(meta) => {
            if let Some(ome) = &meta.ome {
                node["ome"] = json_ome(ome);
            }
            if let Some(spatialdata) = &meta.spatialdata {
                node["spatialdata"] = json!({
                    "kind": spatialdata.kind(),
                    "version": spatialdata.version(),
                });
            }
        }
        NodeKind::Unknown => {}
    }

    // Arrays are leaves here for the same reason they are in the tree: what
    // lies beneath is chunk storage. Their `children` is empty rather than
    // absent, so a reader can walk every node the same way.
    let mut children = Vec::new();
    if !matches!(kind, NodeKind::Array(_)) {
        for path in child_dirs(dir, depth)? {
            children.push(json_tree(
                &node_name(&path),
                &path,
                depth.map(|depth| depth - 1),
            )?);
        }
    }
    node["children"] = Value::Array(children);

    Ok(node)
}

/// An array's fields, with `null` where the tree would print `?`.
///
/// `shards` is the exception to that rule, and is left out altogether rather
/// than written as `null`. The two say different things: `null` means the
/// field was looked for and could not be read, which every array has a shape,
/// chunks and a dtype to be. An unsharded array has no shards to miss, so the
/// key is simply not applicable and does not appear.
fn json_array_meta(meta: &ArrayMeta) -> Value {
    let mut value = json!({
        // Real JSON arrays rather than the `[4096, 4096]` text the tree draws,
        // which is the whole reason `ArrayMeta` keeps the values it was given.
        "shape": meta.shape,
        "chunks": meta.chunks,
        "dtype": meta.dtype,
    });

    // Indexing a `Value` that holds an object inserts the key, which is the
    // shortest way to add a field only sometimes.
    if let Some(shards) = &meta.shards {
        value["shards"] = json!(shards);
    }

    value
}

/// An OME-Zarr image group's metadata.
///
/// `pyramid_levels` is the length of `datasets`, exactly as the tree's row is,
/// so the two numbers cannot drift apart. Both are `null` together when the
/// metadata declares no usable `datasets`.
fn json_ome(ome: &OmeInfo) -> Value {
    json!({
        "tag": ome.tag(),
        "version": ome.version,
        "axes": ome.axes,
        "pyramid_levels": ome.datasets.as_ref().map(|datasets| datasets.len()),
        "datasets": ome.datasets,
    })
}

#[cfg(test)]
mod tests {
    // `tests` is a child module of the crate root, so it can already see the
    // root's private items. This glob import just brings their names into
    // scope so we can call them unqualified.
    use super::*;
    use serde_json::json;

    /// A command line, as `parse_args` wants it: everything after the program
    /// name, owned. `env::args` hands back `String`s, so the tests do too.
    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| String::from(*item)).collect()
    }

    /// The text the tree draws for a dimension list, so that tests about
    /// `shape` and `chunks` keep asserting on what a reader sees rather than
    /// on how the values happen to be stored.
    fn shown(dims: &Option<Vec<Value>>) -> Option<String> {
        dims.as_deref().map(format_dims)
    }

    /// The same idea for axis names, which the tree joins into one row.
    fn joined(names: &Option<Vec<String>>) -> Option<String> {
        names.as_ref().map(|names| names.join(", "))
    }

    #[test]
    fn parse_args_reads_a_directory_and_an_optional_depth() {
        let Ok(Request::Walk(options)) = parse_args(&args(&["store.zarr"])) else {
            panic!("a lone directory is a valid command line");
        };
        assert_eq!(options.path, "store.zarr");
        // No `--depth` means no limit, which is what every invocation before
        // the option existed asked for.
        assert_eq!(options.depth, None);

        // Either order: the option before the directory, or after it.
        for line in [
            ["--depth", "2", "store.zarr"],
            ["store.zarr", "--depth", "2"],
        ] {
            let Ok(Request::Walk(options)) = parse_args(&args(&line)) else {
                panic!("{line:?} is a valid command line");
            };
            assert_eq!(options.path, "store.zarr");
            assert_eq!(options.depth, Some(2));
        }

        // Zero is a depth like any other, and means the root on its own.
        let Ok(Request::Walk(options)) = parse_args(&args(&["--depth", "0", "store.zarr"])) else {
            panic!("zero is a valid depth");
        };
        assert_eq!(options.depth, Some(0));
    }

    #[test]
    fn parse_args_answers_a_flag_wherever_it_appears() {
        // Answered on sight, so a flag beside a directory is still a flag.
        assert!(matches!(
            parse_args(&args(&["store.zarr", "--help"])),
            Ok(Request::Help)
        ));
        assert!(matches!(parse_args(&args(&["-V"])), Ok(Request::Version)));
    }

    #[test]
    fn parse_args_rejects_a_command_line_it_cannot_use() {
        // Every rejection names what was wrong, and none of them panics.
        for (line, expected) in [
            (vec!["--depth"], "--depth needs a number"),
            // `usize` refuses both of these, so no check of ours has to.
            (vec!["--depth", "-1", "store.zarr"], "whole number"),
            (vec!["--depth", "two", "store.zarr"], "whole number"),
            (vec!["--nope", "store.zarr"], "unknown option"),
            (vec!["one.zarr", "two.zarr"], "exactly one directory"),
            (vec![], "expected a directory"),
        ] {
            let error = parse_args(&args(&line))
                .err()
                .unwrap_or_else(|| panic!("{line:?} should not be accepted"));
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?} for {line:?}"
            );
        }
    }

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

        assert_eq!(shown(&meta.shape), Some(String::from("[4096, 4096]")));
        assert_eq!(shown(&meta.chunks), Some(String::from("[512, 512]")));
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

        assert_eq!(shown(&meta.shape), Some(String::from("[4096, 4096]")));
        assert_eq!(shown(&meta.chunks), Some(String::from("[512, 512]")));
        assert_eq!(meta.dtype, Some(String::from("uint16")));
        // No sharding codec, so the grid shape is the chunk shape and there
        // are no shards to speak of.
        assert_eq!(shown(&meta.shards), None);
    }

    #[test]
    fn v3_metadata_missing_fields_become_none() {
        let value = json!({
            "node_type": "array",
            "shape": [4096, 4096]
        });

        let meta = array_meta_v3(&value);

        assert_eq!(shown(&meta.shape), Some(String::from("[4096, 4096]")));
        assert_eq!(shown(&meta.chunks), None);
        assert_eq!(meta.dtype, None);
    }

    #[test]
    fn a_sharded_v3_array_reads_its_chunks_from_the_codec_not_the_grid() {
        // Under the sharding codec the chunk grid describes the shard, and the
        // chunks are the ones inside it. Reading the grid as the chunk shape
        // is what this test exists to stop.
        let value = json!({
            "node_type": "array",
            "shape": [1024, 1024],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [256, 256] }
            },
            "codecs": [{
                "name": "sharding_indexed",
                "configuration": {
                    "chunk_shape": [64, 64],
                    "codecs": [{ "name": "bytes" }]
                }
            }],
            "data_type": "uint16"
        });

        let meta = array_meta_v3(&value);

        assert_eq!(shown(&meta.chunks), Some(String::from("[64, 64]")));
        assert_eq!(shown(&meta.shards), Some(String::from("[256, 256]")));
    }

    #[test]
    fn an_unreadable_sharding_codec_leaves_the_chunks_missing() {
        // The codec is there, so the grid shape is a shard and nothing else.
        // With no inner shape to read, `chunks` goes missing and shows as `?`
        // -- the grid shape is never borrowed to fill the gap, because doing
        // so would print a shard under the name `chunks` all over again.
        let value = json!({
            "node_type": "array",
            "shape": [1024, 1024],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [256, 256] }
            },
            "codecs": [{ "name": "sharding_indexed" }],
            "data_type": "uint16"
        });

        let meta = array_meta_v3(&value);

        assert_eq!(shown(&meta.chunks), None);
        assert_eq!(shown(&meta.shards), Some(String::from("[256, 256]")));
    }

    #[test]
    fn dimensions_are_read_as_stored_and_rendered_on_demand() {
        let value = json!([128, 256, 256]);
        let read = dims(Some(&value)).expect("a list of dimensions");

        // Kept as the values the file held, so `--json` can hand back a real
        // JSON array...
        assert_eq!(read, vec![json!(128), json!(256), json!(256)]);
        // ...and rendered into the tree's text only when the tree asks.
        assert_eq!(format_dims(&read), "[128, 256, 256]");

        // Entries are copied rather than interpreted, so a malformed list
        // survives to the output instead of being dropped on the way.
        let malformed = json!([1, "x", null]);
        let read = dims(Some(&malformed)).expect("a list of three entries");
        assert_eq!(format_dims(&read), "[1, \"x\", null]");

        // Missing, or present but not a list: nothing to show.
        assert_eq!(dims(None), None);
        let not_a_list = json!("4096");
        assert_eq!(dims(Some(&not_a_list)), None);
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

        assert_eq!(shown(&meta.shape), Some(String::from("[10]")));
        assert_eq!(shown(&meta.chunks), Some(String::from("[10]")));
        assert_eq!(meta.dtype, Some(String::from("<M8[ns]")));
    }

    #[test]
    fn ome_version_is_read_from_v3_attributes() {
        // A V3 image group: the OME-Zarr keys sit under their own `ome`
        // namespace inside `attributes`, with the version alongside them. The
        // axes belong to the multiscale rather than to the group.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "multiscales": [
                        {
                            "axes": [
                                { "name": "c", "type": "channel" },
                                { "name": "y", "type": "space", "unit": "micrometer" },
                                { "name": "x", "type": "space", "unit": "micrometer" }
                            ],
                            "datasets": [{ "path": "0" }]
                        }
                    ]
                }
            }
        });

        let info = ome_info_v3(&value).expect("a V3 image group should be recognised");

        assert_eq!(info.tag(), "OME-Zarr 0.5");
        assert_eq!(joined(&info.axes), Some(String::from("c, y, x")));
        assert_eq!(info.datasets, Some(vec![String::from("0")]));
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

        let info = ome_info_v2(&dir);

        fs::remove_dir_all(&dir).unwrap();

        let info = info.expect("multiscales alone should make this an image group");
        assert_eq!(info.tag(), "OME-Zarr");
        // That fixture carries no axes, so there is no row to print.
        assert_eq!(info.axes, None);
        // Its datasets, on the other hand, are laid out the same way in V2 as
        // they are in V3 -- which is why no V2-specific reading was needed.
        assert_eq!(info.datasets, Some(vec![String::from("0")]));
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

        assert!(ome_info_v3(&value).is_none());
    }

    #[test]
    fn axes_are_read_from_the_0_3_string_form() {
        // OME-NGFF 0.3 stores each axis as a bare dimension name.
        let value = json!(["c", "y", "x"]);

        assert_eq!(
            joined(&axis_names(Some(&value))),
            Some(String::from("c, y, x"))
        );
    }

    #[test]
    fn axes_are_read_from_the_0_4_object_form() {
        // 0.4 and 0.5 store an object per axis instead. Only `name` is read,
        // so the result is the very same string the 0.3 form produces above --
        // which is the whole point of formatting them in one place.
        let value = json!([
            { "name": "c", "type": "channel" },
            { "name": "y", "type": "space", "unit": "micrometer" },
            { "name": "x", "type": "space", "unit": "micrometer" }
        ]);

        assert_eq!(
            joined(&axis_names(Some(&value))),
            Some(String::from("c, y, x"))
        );
    }

    #[test]
    fn an_unreadable_axis_entry_keeps_its_place() {
        // The middle entry has no `name`. Dropping it would report a
        // three-dimensional image as two-dimensional, so it becomes `?` and
        // the number of axes still matches what the file declares.
        let value = json!([
            { "name": "y" },
            { "type": "space" },
            { "name": "x" }
        ]);

        assert_eq!(
            joined(&axis_names(Some(&value))),
            Some(String::from("y, ?, x"))
        );
    }

    #[test]
    fn axes_with_nothing_to_show_produce_no_row() {
        // Three ways of having no axes to display, and one rule for all of
        // them: print no row, and never guess one from the arrays.

        // No axes key at all -- OME-NGFF 0.1 and 0.2.
        assert_eq!(axis_names(None), None);

        // Present, but nothing we can walk over.
        let not_an_array = json!("tczyx");
        assert_eq!(axis_names(Some(&not_an_array)), None);

        // Present and walkable, but empty.
        let empty = json!([]);
        assert_eq!(axis_names(Some(&empty)), None);
    }

    #[test]
    fn dataset_paths_are_read_in_declaration_order() {
        // The ordinary case: three levels, named by the usual convention.
        let value = json!([{ "path": "0" }, { "path": "1" }, { "path": "2" }]);

        let paths = dataset_paths(Some(&value)).expect("three declared levels");

        // The level count is just how many the metadata declares.
        assert_eq!(paths.len(), 3);
        assert_eq!(paths.join(", "), "0, 1, 2");
    }

    #[test]
    fn dataset_paths_are_shown_as_stored() {
        // "0", "1", "2" is a convention, not a rule. Anything the file says is
        // a path is printed as it stands.
        let value = json!([{ "path": "full" }, { "path": "half" }]);

        let paths = dataset_paths(Some(&value)).expect("two declared levels");

        assert_eq!(paths.len(), 2);
        assert_eq!(paths.join(", "), "full, half");
    }

    #[test]
    fn an_unreadable_dataset_entry_keeps_its_place() {
        // The middle entry has no `path`. Dropping it would report a
        // three-level pyramid as two-level, so it becomes `?` and the count
        // still matches what the file declares -- the same rule the axes
        // follow.
        let value = json!([{ "path": "0" }, { "foo": "bar" }, { "path": "2" }]);

        let paths = dataset_paths(Some(&value)).expect("three declared levels");

        assert_eq!(paths.len(), 3);
        assert_eq!(paths.join(", "), "0, ?, 2");
    }

    #[test]
    fn datasets_with_nothing_to_show_produce_no_rows() {
        // Three ways of having no datasets to display, and one rule for all of
        // them: print no rows, and never count the child directories instead.

        // No datasets key at all.
        assert_eq!(dataset_paths(None), None);

        // Present, but nothing we can walk over.
        let not_an_array = json!("0");
        assert_eq!(dataset_paths(Some(&not_an_array)), None);

        // Present and walkable, but empty.
        let empty = json!([]);
        assert_eq!(dataset_paths(Some(&empty)), None);
    }

    #[test]
    fn spatialdata_root_is_detected_from_v3_attributes() {
        // The root of a SpatialData store as written today: the marker sits in
        // the group's attributes and carries two versions -- the container
        // format's, which is what we show, and the writing software's, which
        // is what tells a root from an element.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.3"
                }
            }
        });

        // Compared through `tag()` rather than by variant: that keeps the
        // assertion about what the user sees, and saves `SpatialData` from
        // deriving PartialEq and Debug that nothing outside these tests uses.
        assert_eq!(
            spatialdata_info_v3(&value).map(|info| info.tag()),
            Some(String::from("SpatialData 0.2"))
        );
    }

    #[test]
    fn spatialdata_root_without_a_version_is_still_detected() {
        // The discriminator is here, so this is a store root; the version is
        // not, so the tag is bare. Requiring the version would mean refusing
        // to name a store we can plainly see -- the same rule an OME-Zarr
        // image with no readable version follows.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": { "spatialdata_software_version": "0.7.3" }
            }
        });

        assert_eq!(
            spatialdata_info_v3(&value).map(|info| info.tag()),
            Some(String::from("SpatialData"))
        );
    }

    #[test]
    fn an_image_element_is_not_mistaken_for_a_store_root() {
        // An image element from inside a store. It carries a
        // `spatialdata_attrs` of its own -- as every image, label, point and
        // shape element does -- holding only the version of its own encoding.
        // What it does not carry is the software version, and that is the
        // entire difference between an element and a store root.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5-dev-spatialdata",
                    "multiscales": [
                        {
                            "axes": [{ "name": "y" }, { "name": "x" }],
                            "datasets": [{ "path": "s0" }]
                        }
                    ]
                },
                "spatialdata_attrs": { "version": "0.3" }
            }
        });

        // Reported as the element it is, and never with a version: a bare
        // "SpatialData 0.3" here would read as a second store nested inside
        // the first.
        assert_eq!(
            spatialdata_info_v3(&value).map(|info| info.tag()),
            Some(String::from("SpatialData image"))
        );

        // And the OME-Zarr reading is untouched: this is still the image it
        // says it is, with all its rows.
        assert!(ome_info_v3(&value).is_some());
    }

    #[test]
    fn a_labels_element_is_told_from_an_image_by_its_ome_metadata() {
        // Two raster elements from the same store, alike in every way this
        // tool reads except one: the segmentation carries an `image-label`
        // object beside its `multiscales`. That key, and not the `labels/`
        // directory it happens to sit in, is the whole distinction.
        let attributes = |extra: Value| {
            let mut ome = json!({
                "version": "0.5-dev-spatialdata",
                "multiscales": [
                    {
                        "axes": [{ "name": "y" }, { "name": "x" }],
                        "datasets": [{ "path": "scale0" }]
                    }
                ]
            });
            for (key, value) in extra.as_object().unwrap() {
                ome[key] = value.clone();
            }
            json!({
                "zarr_format": 3,
                "node_type": "group",
                "attributes": {
                    "ome": ome,
                    "spatialdata_attrs": { "version": "0.3" }
                }
            })
        };

        let image = attributes(json!({}));
        let labels = attributes(json!({ "image-label": { "version": "0.5" } }));

        assert_eq!(
            spatialdata_info_v3(&image).map(|info| info.tag()),
            Some(String::from("SpatialData image"))
        );
        assert_eq!(
            spatialdata_info_v3(&labels).map(|info| info.tag()),
            Some(String::from("SpatialData labels"))
        );

        // Both are OME-Zarr images as far as the rest of the tool cares, so
        // neither loses a row for being classified.
        assert!(ome_info_v3(&image).is_some());
        assert!(ome_info_v3(&labels).is_some());
    }

    #[test]
    fn a_plain_ome_zarr_image_is_not_a_spatialdata_element() {
        // The same multiscale metadata, without SpatialData's mark on it: an
        // ordinary microscopy image that has nothing to do with SpatialData.
        // Classifying rasters from OME-Zarr alone would tag every one of them.
        let value = json!({
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
        });

        assert!(spatialdata_info_v3(&value).is_none());
        assert!(ome_info_v3(&value).is_some());
    }

    #[test]
    fn a_raster_element_is_read_from_a_v2_zattrs_file() {
        let dir = env::temp_dir().join(format!("zarr-tree-test-sd-labels-v2-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        // A Zarr V2 segmentation. V2 predates the `ome` namespace, so
        // `image-label` and `multiscales` sit at the top level of `.zattrs`
        // beside `spatialdata_attrs` -- which is the whole reason the two
        // objects are passed separately.
        fs::write(dir.join(".zgroup"), r#"{"zarr_format": 2}"#).unwrap();
        fs::write(
            dir.join(".zattrs"),
            r#"{
                "image-label": {"version": "0.4"},
                "multiscales": [{"version": "0.4", "datasets": [{"path": "0"}]}],
                "spatialdata_attrs": {"version": "0.2"}
            }"#,
        )
        .unwrap();

        let info = spatialdata_info_v2(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            info.map(|info| info.tag()),
            Some(String::from("SpatialData labels"))
        );
    }

    #[test]
    fn a_group_with_spatialdata_attrs_but_no_raster_metadata_is_not_an_element() {
        // Two ways of carrying SpatialData's mark without being a raster. Both
        // leave the group untagged rather than guessing which element it is.

        // No OME-Zarr metadata at all.
        let no_ome = json!({ "attributes": { "spatialdata_attrs": { "version": "0.3" } } });
        assert!(spatialdata_info_v3(&no_ome).is_none());

        // The namespace is there, but declares no image.
        let no_multiscales = json!({
            "attributes": {
                "ome": { "version": "0.5" },
                "spatialdata_attrs": { "version": "0.3" }
            }
        });
        assert!(spatialdata_info_v3(&no_multiscales).is_none());
    }

    #[test]
    fn spatialdata_metadata_with_nothing_to_show_produces_no_tag() {
        // Three ways of not being a SpatialData store root, and one rule for
        // all of them: print no tag, and never fall back on directory names.

        // No marker at all -- an ordinary Zarr group.
        let plain = json!({ "zarr_format": 3, "node_type": "group", "attributes": {} });
        assert!(spatialdata_info_v3(&plain).is_none());

        // Present, but not an object. `get` on a string yields None, so this
        // costs no type check of its own.
        let not_an_object = json!({ "attributes": { "spatialdata_attrs": "0.2" } });
        assert!(spatialdata_info_v3(&not_an_object).is_none());

        // An object, and a plausible one, but with no discriminator in it.
        let no_discriminator = json!({ "attributes": { "spatialdata_attrs": { "foo": "bar" } } });
        assert!(spatialdata_info_v3(&no_discriminator).is_none());
    }

    #[test]
    fn spatialdata_root_is_read_from_a_v2_zattrs_file() {
        let dir = env::temp_dir().join(format!("zarr-tree-test-sd-v2-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        // Container format 0.1 is a Zarr V2 store. `.zattrs` *is* the
        // attributes object, so the marker sits at its top level rather than
        // under an `attributes` field the way V3 nests it -- which is the one
        // thing this test can check and the V3 tests above cannot.
        fs::write(dir.join(".zgroup"), r#"{"zarr_format": 2}"#).unwrap();
        fs::write(
            dir.join(".zattrs"),
            r#"{"spatialdata_attrs": {"version": "0.1", "spatialdata_software_version": "0.7.3"}}"#,
        )
        .unwrap();

        let info = spatialdata_info_v2(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            info.map(|info| info.tag()),
            Some(String::from("SpatialData 0.1"))
        );
    }

    #[test]
    fn points_element_is_detected_from_v3_attributes() {
        // A transcripts element, as a current Xenium store writes it: the kind
        // in `encoding-type`, and beside it the scientific metadata this
        // version deliberately does not display.
        let value = json!({
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
        });

        let info = spatialdata_info_v3(&value).expect("an element marker should be recognised");

        // No version in the tag: the 0.2 above is this element's own encoding
        // version, not the container version a root line shows.
        assert_eq!(info.tag(), "SpatialData points");
    }

    #[test]
    fn shapes_element_is_detected_from_v3_attributes() {
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "ngff:shapes",
                "axes": ["x", "y"],
                "coordinateTransformations": [],
                "spatialdata_attrs": { "version": "0.3" }
            }
        });

        let info = spatialdata_info_v3(&value).expect("an element marker should be recognised");

        assert_eq!(info.tag(), "SpatialData shapes");
    }

    #[test]
    fn an_element_is_recognised_without_spatialdata_attrs() {
        // The rule is the `encoding-type` value and nothing else. Every other
        // points and shapes fixture in this file carries a `spatialdata_attrs`
        // beside it, because a store written today does -- which could leave a
        // reader believing that object is part of what is being matched.
        //
        // It is not, and the difference shows up in the oldest stores. One
        // written before SpatialData recorded a software version has no root
        // marker at all; requiring `spatialdata_attrs` here would leave its
        // elements untagged as well, and the whole store would go unrecognised.
        // Rasters are the ones that do need it -- see
        // `spatialdata_raster_element`, which has nothing else to go on -- and
        // the two rules are deliberately separate.
        for (marker, expected) in [
            ("ngff:points", "SpatialData points"),
            ("ngff:shapes", "SpatialData shapes"),
        ] {
            let value = json!({
                "zarr_format": 3,
                "node_type": "group",
                "attributes": { "encoding-type": marker }
            });

            assert_eq!(
                spatialdata_info_v3(&value).map(|info| info.tag()),
                Some(String::from(expected)),
                "{marker:?} names its own kind, with nothing beside it"
            );
        }
    }

    #[test]
    fn a_shapes_element_from_the_array_era_is_still_detected() {
        let dir = env::temp_dir().join(format!("zarr-tree-test-sd-shapes-v1-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();

        // The oldest shapes encoding, from a Zarr V2 store: the geometry lived
        // in sibling Zarr arrays rather than in a Parquet file, `geos` recorded
        // its type, and `axes` was written as JSON null.
        //
        // None of that changes the marker, which is the whole point of this
        // test: recognition reads one key that every release has written the
        // same way, so a format change we never look at cannot reach us.
        fs::write(dir.join(".zgroup"), r#"{"zarr_format": 2}"#).unwrap();
        fs::write(
            dir.join(".zattrs"),
            r#"{
                "axes": null,
                "coordinateTransformations": [],
                "encoding-type": "ngff:shapes",
                "spatialdata_attrs": {
                    "geos": {"name": "POLYGON", "type": 3},
                    "version": "0.1"
                }
            }"#,
        )
        .unwrap();

        let info = spatialdata_info_v2(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            info.map(|info| info.tag()),
            Some(String::from("SpatialData shapes"))
        );
    }

    #[test]
    fn a_table_element_is_detected_beside_its_anndata_metadata() {
        // A table group as every release has written it. AnnData wrote the
        // first two keys and claims `encoding-type` for itself, which is why
        // SpatialData records the element kind one key over.
        let value = json!({
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
        });

        let info = spatialdata_info_v3(&value).expect("a table marker should be recognised");

        // Recognised despite `encoding-type` saying "anndata": the two keys
        // are read independently, and neither shadows the other.
        assert_eq!(info.tag(), "SpatialData table");
    }

    #[test]
    fn anndata_nodes_beneath_a_table_are_not_elements() {
        // AnnData writes `encoding-type` throughout the subtree under a table.
        // None of these is a SpatialData element, and matching the key rather
        // than its value would tag every one of them.
        for kind in ["anndata", "dataframe", "csr_matrix", "array", "dict"] {
            let value = json!({ "attributes": { "encoding-type": kind } });
            assert!(
                spatialdata_info_v3(&value).is_none(),
                "{kind:?} is AnnData's, not an element kind"
            );
        }
    }

    #[test]
    fn unrecognised_encoding_types_produce_no_element_tag() {
        // Four ways of not being an element we know, and one rule for all of
        // them: print no element tag, and never fall back on directory names.

        // Absent -- an ordinary Zarr group.
        let plain = json!({ "attributes": {} });
        assert!(spatialdata_info_v3(&plain).is_none());

        // A real value from a real store, and not ours: every SpatialData
        // annotation table carries this, written by AnnData. Matching the key
        // rather than the value would tag all of them as elements.
        let anndata = json!({ "attributes": { "encoding-type": "anndata" } });
        assert!(spatialdata_info_v3(&anndata).is_none());

        // A kind we do not know. Not guessed at, not reported.
        let unknown = json!({ "attributes": { "encoding-type": "ngff:something-new" } });
        assert!(spatialdata_info_v3(&unknown).is_none());

        // Present, but not a string. `as_str` yields None, so this costs no
        // type check of its own.
        let not_a_string = json!({ "attributes": { "encoding-type": 7 } });
        assert!(spatialdata_info_v3(&not_a_string).is_none());
    }

    #[test]
    fn a_root_marker_outranks_an_element_marker() {
        // Contradictory metadata that no writer produces: the discriminator
        // that makes a store root, and beside it an element kind. A group is
        // one or the other and never both, so `spatialdata_info` has to choose
        // -- and it chooses the root, because `spatialdata_root` is simply
        // tried first.
        //
        // That ordering is the entire rule, which is why it is worth a test of
        // its own. Swapping the two `or_else` arms would change what this
        // prints, and nothing else in the suite would notice.
        //
        // The root wins on two grounds. Its marker is the stricter of the two
        // -- a key nested inside an object, rather than one string at the top
        // level -- so given input that cannot be trusted, it is the claim less
        // likely to have been made by accident. And reading a root as an
        // element would lose both the container version and the fact that this
        // node is the store, where the reverse loses only a kind word.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "spatialdata_attrs": {
                    "version": "0.2",
                    "spatialdata_software_version": "0.7.3"
                },
                "encoding-type": "ngff:points"
            }
        });

        assert_eq!(
            spatialdata_info_v3(&value).map(|info| info.tag()),
            Some(String::from("SpatialData 0.2"))
        );
    }
}
