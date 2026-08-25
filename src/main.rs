use std::cell::Cell;
use std::env;
use std::fs;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::process;

use object_store::aws::AmazonS3Builder;
use object_store::http::HttpBuilder;
use object_store::path::Path as ObjectPath;
use object_store::{ClientConfigKey, ObjectStore, ObjectStoreExt};
use serde_json::{Value, json};
use tokio::runtime::Runtime;
use url::Url;

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

/// Which kind of OME-Zarr group this is.
///
/// The three are told apart by which key the metadata carries -- `multiscales`,
/// `plate` or `well` -- and never by what the group or its children are called.
/// A plate's wells really are named `A`, `B`, `1`, `2`, and a store is free to
/// use those names for anything at all.
enum OmeKind {
    /// A multiscale image: the only kind this tool recognised before HCS, and
    /// the only one that carries axes and datasets.
    Image,
    /// A high-content-screening plate. The three counts are the lengths of the
    /// lists the metadata declares, each `None` on its own when that list is
    /// missing or is not a list -- nothing here is counted from the directories
    /// on disk, so a plate that declares 96 wells says 96 whether or not 96
    /// were written.
    Plate {
        rows: Option<usize>,
        columns: Option<usize>,
        wells: Option<usize>,
    },
    /// A single well of a plate. Tagged and nothing more: what a well holds is
    /// its images, and those are the child groups the tree already prints.
    Well,
}

impl OmeKind {
    /// The word that follows the version in a label, or `None` for an image.
    ///
    /// An image keeps the bare `OME-Zarr 0.5` it has always had, which is what
    /// leaves ordinary image output untouched by any of this.
    fn suffix(&self) -> Option<&'static str> {
        match self {
            OmeKind::Image => None,
            OmeKind::Plate { .. } => Some("plate"),
            OmeKind::Well => Some("well"),
        }
    }

    /// The kind on its own, for output that has already said it is talking
    /// about OME-Zarr.
    fn name(&self) -> &'static str {
        match self {
            OmeKind::Image => "image",
            OmeKind::Plate { .. } => "plate",
            OmeKind::Well => "well",
        }
    }
}

/// What an OME-Zarr group carries.
///
/// The `Option` sits outside this struct rather than around its fields: a group
/// either is an OME-Zarr group of some kind, in which case it always has a kind
/// and at least a version slot, or it is an ordinary group and there is nothing
/// here at all.
struct OmeInfo {
    /// Which of the three kinds this is. See `OmeKind`.
    kind: OmeKind,
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
    /// The label tag: "OME-Zarr 0.5", or "OME-Zarr" when no version was read,
    /// with the kind appended for a plate or a well -- "OME-Zarr 0.5 plate".
    ///
    /// Built here rather than stored, so that the version and the text made
    /// from it cannot drift apart -- the same reason `pyramid levels` is the
    /// length of `datasets` rather than a number of its own.
    fn tag(&self) -> String {
        let mut tag = match &self.version {
            Some(version) => format!("OME-Zarr {version}"),
            None => String::from("OME-Zarr"),
        };

        if let Some(suffix) = self.kind.suffix() {
            tag.push(' ');
            tag.push_str(suffix);
        }

        tag
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
Explore the structure and metadata of a Zarr store.

USAGE:
    zarr-tree [OPTIONS] <STORE>

STORE is a directory on this machine, an S3 URI, or an HTTP(S) URL:

    zarr-tree /data/store.zarr
    zarr-tree s3://bucket/path/store.zarr
    zarr-tree https://server.example/data/store.zarr

S3 settings are read from the usual AWS_* environment variables. When none of
them supplies a credential, requests are sent unsigned, which is what a public
bucket wants; set AWS_SKIP_SIGNATURE=false to force the credential chain, and
AWS_REGION when the bucket is not in us-east-1.

Walking an HTTP(S) store needs a server that answers WebDAV PROPFIND, which is
how children are found. Metadata is read with ordinary GETs, so a static server
can still be inspected with --depth 0.

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
const USAGE: &str = "usage: zarr-tree [OPTIONS] <STORE>";

/// What the command line asked for.
struct Options {
    /// The store to walk, exactly as it was typed: a directory on this
    /// machine, or an `s3://` URI.
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

    // Which kind of store this is is settled once, here, by the scheme on the
    // path. Everything below reaches it through `Store` and never asks again.
    let store = open_store(&options.path)?;

    // The root is named by the path as it was typed, in both outputs. Every
    // node below it is named by its directory, or by its S3 prefix.
    let root_name = options.path.trim_end_matches('/');

    // Classified before the root is checked, because the check wants the
    // answer: a root whose metadata named it a Zarr node is plainly there, and
    // one that named nothing has to be looked for. See `Store::check_root`.
    let root_kind = classify(store.as_ref(), "");
    store.check_root(!matches!(root_kind, NodeKind::Unknown))?;

    if options.json {
        let tree = json_tree(store.as_ref(), "", root_name, root_kind, options.depth)?;
        // Indented rather than compact: this is still a command a person runs
        // and reads, and `jq` does not mind either way.
        writeln!(out, "{}", serde_json::to_string_pretty(&tree)?)?;
        return out.flush();
    }

    print_store(
        &mut out,
        store.as_ref(),
        root_name,
        &root_kind,
        options.depth,
    )?;

    // Stdout flushes itself when the process ends, but it swallows any error
    // in doing so. Flushing here is what puts a late `BrokenPipe` in front of
    // the handler above instead of losing it.
    out.flush()
}

/// Read the command line, or say what is wrong with it.
///
/// `args` is everything after the program name. Hand-written rather than
/// reached for a crate: three options and one store is a short enough grammar
/// that a loop over the arguments says all there is to say. Which kind of
/// store the argument names is not decided here -- see `parse_location`.
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
            // path. The cost of reading it that way is that a directory whose
            // name begins with `-` can no longer be inspected -- the same
            // trade `-h` and `-V` already made.
            other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
            other => {
                if path.is_some() {
                    return Err(String::from("expected exactly one store"));
                }
                path = Some(other);
            }
        }
    }

    let path = path.ok_or_else(|| String::from("expected a store"))?;
    Ok(Request::Walk(Options {
        path: String::from(path),
        depth,
        json,
    }))
}

/// Where a walk reads its metadata from.
///
/// Two things implement it: a directory on this machine, and a key prefix in
/// an object store -- an S3 bucket, or a path on an HTTP server. Everything
/// above it -- the walk, both renderers, and every function that interprets
/// Zarr, OME-Zarr or SpatialData metadata -- is written once and never learns
/// which one it is looking at.
///
/// Three methods, because a walk asks only three questions: what does this
/// metadata file say, what lies immediately below this node, and is the root
/// there at all.
///
/// Paths are `/`-separated and relative to the store root, with the empty
/// string for the root itself. Each implementation joins them onto its own
/// base, which is what keeps that base -- a directory here, a bucket and a
/// prefix there -- out of everything above.
trait Store {
    /// The text of the metadata file at `path`, or `None` when it is missing
    /// or unreadable.
    ///
    /// Absence and failure are deliberately not told apart. Every caller
    /// treats an unreadable metadata file the way it treats a missing one, and
    /// answering "no" is the ordinary course of classifying a node: three of
    /// these questions are asked of every directory, and at most one of them
    /// finds a file.
    fn read(&self, path: &str) -> Option<String>;

    /// The names of the immediate children of `path`, sorted.
    ///
    /// Subdirectories here, common prefixes there. A file is a child in
    /// neither: a metadata file is what `read` is for, and a chunk is not part
    /// of the structure at all.
    fn children(&self, path: &str) -> io::Result<Vec<String>>;

    /// Fail if the store root is not there, before anything is printed.
    ///
    /// `identified` says whether the root's own metadata has already named it
    /// a Zarr node. A local store ignores it: `exists()` answers on its own,
    /// for nothing.
    ///
    /// A remote store has no such question to ask, and must fall back on a
    /// listing -- of which there is one it must never make. What lies beneath
    /// an array is chunk objects, and a real store has millions of them.
    /// Metadata that identified the node is proof enough that the root is
    /// there, so the listing is reached only when nothing did, which is
    /// exactly when the prefix cannot be an array.
    fn check_root(&self, identified: bool) -> io::Result<()>;
}

/// The path of `name` inside `parent`, in the form `Store` takes.
///
/// A function rather than a `format!` at each call site because the root is
/// the empty string, and joining onto it must not produce a leading slash.
fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        String::from(name)
    } else {
        format!("{parent}/{name}")
    }
}

/// Where the positional argument points.
///
/// The scheme decides, and only `s3://`, `http://` and `https://` name a
/// remote store. Everything else is a path on this machine and is passed
/// through untouched -- including a relative path that happens to contain one
/// of those somewhere after its start.
enum Location {
    Local,
    /// A bucket, and the key prefix the store root sits at. The prefix is
    /// empty when the URI named a bucket and nothing more.
    S3 {
        bucket: String,
        key: String,
    },
    /// A server, and the path the store root sits at.
    ///
    /// Split the same way S3 is, and for the same reason: `object_store`'s
    /// HTTP store wants one base URL and keys beneath it, and keys are what
    /// come back from a listing. `base` is the origin -- scheme, host, port,
    /// and any query string -- and `path` is everything after it.
    Http {
        base: String,
        path: String,
    },
}

/// Read a remote URI, or say what is wrong with it.
///
/// Anything without a scheme this understands is a local path, returned
/// unread: a directory is whatever the filesystem says it is, and nothing here
/// should have an opinion about its spelling.
fn parse_location(target: &str) -> Result<Location, String> {
    if let Some(rest) = target.strip_prefix("s3://") {
        // A trailing slash is how a person writes a prefix, and an S3 key has
        // no use for one: `s3://bucket/store.zarr/` and `s3://bucket/store.zarr`
        // name the same place. The root line trims the same slash off the local
        // spelling for the same reason.
        let rest = rest.trim_end_matches('/');
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));

        if bucket.is_empty() {
            return Err(format!("expected s3://bucket/prefix, not {target:?}"));
        }

        return Ok(Location::S3 {
            bucket: String::from(bucket),
            key: String::from(key),
        });
    }

    if target.starts_with("http://") || target.starts_with("https://") {
        return parse_http_location(target);
    }

    Ok(Location::Local)
}

/// Split an `http://` or `https://` URI into the base URL and the path
/// beneath it.
///
/// Parsed rather than sliced. `object_store`'s HTTP store builds every request
/// URL by extending its base URL's path segments, which percent-encodes each
/// segment as it goes -- so the path it is given has to be *decoded*, and
/// `Path::from_url_path` is what decodes it. Splitting the URI by hand would
/// have meant deciding all of that here, wrongly.
///
/// The base is the origin and nothing else, because a listing answers with
/// paths from the server root rather than from wherever the store happens to
/// begin. Keeping the base at the root is what makes a read and a listing
/// agree about what a key means.
///
/// A query string rides on the base, so it survives onto every request -- the
/// one shape of access token a static server tends to want. A fragment is
/// dropped: it never reaches a server anyway.
fn parse_http_location(target: &str) -> Result<Location, String> {
    // `Url::parse` is the whole of the validation. For `http` and `https` it
    // requires a host, so `https://` and `http://?token=x` are refused here
    // and nothing needs checking afterwards.
    let url = Url::parse(target).map_err(|error| format!("invalid url {target:?}: {error}"))?;

    let path = ObjectPath::from_url_path(url.path())
        .map_err(|error| format!("invalid url path in {target:?}: {error}"))?;

    let mut base = url.clone();
    base.set_path("/");
    base.set_fragment(None);

    Ok(Location::Http {
        base: base.to_string(),
        path: String::from(path.as_ref()),
    })
}

/// Open the store the command line named.
///
/// Nothing here reaches the network or the filesystem: this only decides what
/// to build. Whether the root is really there is `Store::check_root`'s
/// question, asked once the walk knows what the root claims to be.
fn open_store(target: &str) -> io::Result<Box<dyn Store>> {
    match parse_location(target).map_err(io::Error::other)? {
        Location::Local => Ok(Box::new(LocalStore::new(target))),
        Location::S3 { bucket, key } => Ok(Box::new(RemoteStore::s3(target, &bucket, &key)?)),
        Location::Http { base, path } => Ok(Box::new(RemoteStore::http(target, &base, &path)?)),
    }
}

/// A Zarr store in a directory on this machine.
struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    fn new(root: &str) -> Self {
        LocalStore {
            root: PathBuf::from(root),
        }
    }

    /// Where on disk a store path lands.
    fn resolve(&self, path: &str) -> PathBuf {
        if path.is_empty() {
            self.root.clone()
        } else {
            // `join` takes the `/`-separated form as it stands: `Path` splits
            // on the separator itself, so no component-by-component walk of
            // our own is needed.
            self.root.join(path)
        }
    }
}

impl Store for LocalStore {
    fn read(&self, path: &str) -> Option<String> {
        // `.ok()` drops the error and leaves an Option: the caller cares
        // *that* this failed, not why.
        fs::read_to_string(self.resolve(path)).ok()
    }

    fn children(&self, path: &str) -> io::Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(self.resolve(path))? {
            let entry = entry?;
            // file_type() does not follow symlinks, so a link pointing back at
            // an ancestor cannot send us into infinite recursion.
            if entry.file_type()?.is_dir() {
                // A name that is not valid UTF-8 keeps its readable parts,
                // with the rest replaced, which is what `to_string_lossy` is
                // for.
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        // `read_dir` returns entries in arbitrary order, and the tree has to
        // know which child is last before it can draw a connector.
        names.sort();
        Ok(names)
    }

    fn check_root(&self, _identified: bool) -> io::Result<()> {
        // The two questions the filesystem answers for nothing, and the reason
        // this method takes an argument it ignores: only a remote store has to
        // pay for the answer.
        if !self.root.exists() {
            return Err(io::Error::other(format!(
                "path does not exist: {}",
                self.root.display()
            )));
        }
        if !self.root.is_dir() {
            return Err(io::Error::other(format!(
                "path is not a directory: {}",
                self.root.display()
            )));
        }
        Ok(())
    }
}

/// Which object store is behind a `RemoteStore`.
///
/// The walk never asks: both answer `read` and `children` the same way, which
/// is the whole point of `Store`. Their *failures* differ enough to be worth
/// telling apart, and that is the only thing this is read for -- see
/// `RemoteStore::diagnose`.
#[derive(Clone, Copy, PartialEq)]
enum Backend {
    S3,
    Http,
}

impl Backend {
    /// What to call a root that is not there.
    ///
    /// One request cannot tell a missing bucket from a missing key, so S3 says
    /// both. An HTTP URL has no such split.
    fn missing(&self, uri: &str) -> String {
        match self {
            Backend::S3 => format!("no such bucket or prefix: {uri}"),
            Backend::Http => format!("not found: {uri}"),
        }
    }
}

/// A Zarr store held under a key prefix in an object store.
///
/// `s3` and `http` build the two this program uses. The tests build the same
/// type over `object_store`'s in-memory store, which is what lets the remote
/// walk be tested with no network, no mock server and no AWS account.
struct RemoteStore {
    /// The runtime the requests are driven on. `object_store` is async
    /// throughout and zarr-tree is not, so every call below is run to
    /// completion here.
    ///
    /// A current-thread runtime, because one request at a time is exactly the
    /// shape the walk already had. Built once and kept: a runtime per request
    /// would mean a connection pool per request too.
    runtime: Runtime,
    store: Box<dyn ObjectStore>,
    /// The key prefix the store root sits at, empty for a whole bucket.
    prefix: ObjectPath,
    /// The URI as it was typed. Only the error messages read it.
    uri: String,
    /// Which store this is. Only the error messages read it.
    backend: Backend,
    /// Whether any read has yet come back with something.
    ///
    /// Evidence, kept for one message. An HTTP listing is a WebDAV
    /// `PROPFIND`, which an ordinary static server answers "method not
    /// allowed" -- and `object_store` hands that back as the same
    /// unclassified error a dead host would produce. One read having already
    /// succeeded is what separates the two, and it is proof rather than a
    /// guess: the metadata we are printing came off that very server.
    ///
    /// A `Cell` because `read` takes `&self`, as every `Store` method does.
    /// Nothing here is threaded, and nothing else reads this.
    reachable: Cell<bool>,
}

impl RemoteStore {
    /// Wrap an object store, with `prefix` as the store root.
    fn new(
        store: impl ObjectStore + 'static,
        prefix: &str,
        uri: &str,
        backend: Backend,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        Ok(RemoteStore {
            runtime,
            store: Box::new(store),
            prefix: ObjectPath::from(prefix),
            uri: String::from(uri),
            backend,
            reachable: Cell::new(false),
        })
    }

    /// An S3 bucket, configured from the environment.
    ///
    /// `AmazonS3Builder::from_env` reads the `AWS_*` variables -- region,
    /// endpoint, access key, session token, and the web-identity and container
    /// credential settings -- and nothing else is configured here. There is no
    /// credential store of ours, no prompt and no profile manager: what the
    /// environment says is what is used, and what it does not say is left to
    /// the library's own chain.
    ///
    /// One thing that chain does not include: `object_store` does not read
    /// `~/.aws/credentials`, so a named profile there has no effect on its
    /// own. `aws configure export-credentials --format env` is the bridge.
    fn s3(uri: &str, bucket: &str, key: &str) -> io::Result<Self> {
        let mut builder = AmazonS3Builder::from_env().with_bucket_name(bucket);

        if anonymous_by_default() {
            builder = builder.with_skip_signature(true);
        }

        let store = builder
            .build()
            .map_err(|error| remote_error(Backend::S3, uri, &error))?;
        RemoteStore::new(store, key, uri, Backend::S3)
    }

    /// A path on an HTTP server.
    ///
    /// `base` is the origin, and every key is a path beneath it -- see
    /// `parse_http_location` for why the split falls there. Nothing is
    /// configured beyond that: there are no credentials, no headers and no
    /// options of ours, so what reaches the server is a plain `GET` and, for a
    /// listing, a WebDAV `PROPFIND`.
    ///
    /// `http://` is allowed only because the URI asked for it in so many
    /// words. `object_store` refuses cleartext by default, which is the right
    /// default for a URL it was handed rather than typed.
    fn http(uri: &str, base: &str, path: &str) -> io::Result<Self> {
        let mut builder = HttpBuilder::new().with_url(base);

        if base.starts_with("http://") {
            builder = builder.with_config(ClientConfigKey::AllowHttp, "true");
        }

        let store = builder
            .build()
            .map_err(|error| remote_error(Backend::Http, uri, &error))?;
        RemoteStore::new(store, path, uri, Backend::Http)
    }

    /// The object key a store path lands on.
    fn key(&self, path: &str) -> ObjectPath {
        // `ObjectPath::from` splits on `/` and drops the empty parts, so the
        // root's empty path and a bucket with no prefix both come out right
        // without a case of their own.
        ObjectPath::from(format!("{}/{path}", self.prefix))
    }

    /// Why a listing failed, in one line.
    ///
    /// A failed *listing* is the one thing `object_store` hands back
    /// unclassified: whatever went wrong, it arrives as a generic error with
    /// the server's own explanation attached, and the status that would have
    /// sorted it is not reachable from outside the library. A failed *read* is
    /// sorted, so a read is what this asks with.
    ///
    /// The two stores want different answers from that, because the listing
    /// they failed at is a different operation. On S3 it is `ListObjectsV2`,
    /// which every bucket supports, so a failure means the name was wrong. On
    /// HTTP it is a WebDAV `PROPFIND`, which an ordinary static server does
    /// not implement at all -- and telling somebody their store does not exist
    /// when we have just printed metadata read from it would be plainly wrong.
    ///
    /// So on HTTP, any evidence that the server is there and serving files
    /// settles it: either a read has already succeeded, or a read of this very
    /// prefix does now.
    fn diagnose(&self, error: &object_store::Error) -> io::Error {
        let cannot_list = || {
            io::Error::other(format!(
                "cannot list {}: the server answers GET but not the WebDAV \
                 listing needed to find child nodes",
                self.uri
            ))
        };

        // Some listing failures do arrive sorted -- a WebDAV `PROPFIND` that
        // was refused, rather than not understood. Those say what they are and
        // need no second request.
        if let object_store::Error::NotFound { .. }
        | object_store::Error::PermissionDenied { .. }
        | object_store::Error::Unauthenticated { .. } = error
        {
            return remote_error(self.backend, &self.uri, error);
        }

        let http = self.backend == Backend::Http;
        if http && self.reachable.get() {
            return cannot_list();
        }

        // One extra request, on the failing path and only there.
        match self.runtime.block_on(self.store.head(&self.prefix)) {
            // A resource that answers a read but not a listing. On S3 that
            // proves nothing -- a key prefix is not an object, so reading one
            // never succeeds -- but on HTTP it is the whole story.
            Ok(_) if http => cannot_list(),
            Ok(_) | Err(object_store::Error::NotFound { .. }) => {
                io::Error::other(self.backend.missing(&self.uri))
            }
            Err(error) => remote_error(self.backend, &self.uri, &error),
        }
    }

    /// The immediate children and the objects of one prefix, in one request.
    fn list(&self, prefix: &ObjectPath) -> io::Result<object_store::ListResult> {
        // On S3 one `ListObjectsV2` with `delimiter=/`, on HTTP one WebDAV
        // `PROPFIND` with `Depth: 1`. Both answer with the keys directly under
        // the prefix and one entry per child collection beneath it -- so a
        // child costs a line of a response rather than a request of its own.
        self.runtime
            .block_on(self.store.list_with_delimiter(Some(prefix)))
            .map_err(|error| self.diagnose(&error))
    }
}

impl Store for RemoteStore {
    fn read(&self, path: &str) -> Option<String> {
        let key = self.key(path);
        let bytes = self
            .runtime
            .block_on(async { self.store.get(&key).await.ok()?.bytes().await.ok() })?;

        // Noted for one error message, and nothing else. See `reachable`.
        self.reachable.set(true);

        String::from_utf8(bytes.to_vec()).ok()
    }

    fn children(&self, path: &str) -> io::Result<Vec<String>> {
        let listing = self.list(&self.key(path))?;

        // Only the common prefixes. The objects in the same response are this
        // node's own metadata files and, beneath an unrecognised node, its
        // chunks -- neither of which is a child.
        //
        // This is the only place a listing is made, and the walk stops at an
        // array before reaching it. That is what keeps a store's chunk objects
        // out of the walk entirely: they are never listed, so they are never
        // named, however many millions of them there are.
        let mut names: Vec<String> = listing
            .common_prefixes
            .iter()
            .filter_map(|prefix| prefix.filename())
            .map(String::from)
            .collect();
        names.sort();
        Ok(names)
    }

    fn check_root(&self, identified: bool) -> io::Result<()> {
        // Metadata that named the root a group or an array has already proved
        // the bucket is reachable and the prefix is real, at no cost: the
        // reads that answered are the ones `classify` had to make anyway.
        if identified {
            return Ok(());
        }

        // Nothing identified it, so ask. This is safe here for the reason it
        // is safe nowhere else: a prefix with no `.zarray` and no array
        // `zarr.json` is not an array, so it is not the prefix full of chunks
        // that `Store::check_root` warns about.
        let listing = self.list(&self.prefix)?;

        if listing.objects.is_empty() && listing.common_prefixes.is_empty() {
            return Err(io::Error::other(self.backend.missing(&self.uri)));
        }
        Ok(())
    }
}

/// Whether requests should go out unsigned unless the environment says
/// otherwise.
///
/// `object_store` resolves credentials from the `AWS_*` variables, then a
/// web-identity token, then a container credential endpoint, and finally the
/// EC2 instance metadata service. Off an EC2 instance that last step cannot
/// succeed, and every request would spend a second failing to reach
/// 169.254.169.254 before returning a signature error -- which is what reading
/// a public bucket, the ordinary case for this tool, would get.
///
/// So when nothing in the environment names a credential, requests are sent
/// unsigned. `AWS_SKIP_SIGNATURE` is left to say otherwise in either
/// direction, and an EC2 instance role is what wants it set to `false`.
fn anonymous_by_default() -> bool {
    if env::var_os("AWS_SKIP_SIGNATURE").is_some() {
        return false;
    }

    ![
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some())
}

/// The one line printed for a failed remote request.
///
/// Four cases, because a person can act on each of the first three: the name
/// is wrong, the credentials are not allowed, the credentials are not valid,
/// or something else went wrong and the library's own account of it is the
/// best there is.
///
/// Nothing here can print a secret. `object_store` puts no key, token or
/// signature in an error, and the fourth case is cut to its first line anyway
/// -- which is also what keeps a failure from spilling a whole XML response
/// across the terminal.
fn remote_error(backend: Backend, uri: &str, error: &object_store::Error) -> io::Error {
    let message = match error {
        object_store::Error::NotFound { .. } => backend.missing(uri),
        object_store::Error::PermissionDenied { .. } => format!("permission denied: {uri}"),
        object_store::Error::Unauthenticated { .. } => format!("authentication failed: {uri}"),
        other => format!("request failed: {}", first_line(other)),
    };
    io::Error::other(message)
}

/// The first line of an error's own words, and no more.
///
/// A failed request carries the whole response body in its message, and both
/// S3 and WebDAV answer in XML across several lines. One line is what belongs
/// on a terminal; the rest was never an explanation a person wanted.
fn first_line(error: &object_store::Error) -> String {
    let message = error.to_string();
    String::from(message.lines().next().unwrap_or_default())
}

/// Draw the whole tree for one store, root line and all.
///
/// Split out of `run` so that the tests can render a store into a buffer of
/// their own: the walk writes through `&mut dyn Write` and neither knows nor
/// cares that this is usually standard output.
fn print_store(
    out: &mut dyn Write,
    store: &dyn Store,
    name: &str,
    kind: &NodeKind,
    depth: Option<usize>,
) -> io::Result<()> {
    writeln!(out, "{name} {}", kind.label())?;

    // An array is a leaf here too: its metadata takes the place of the walk.
    match kind {
        NodeKind::Array(meta) => print_array_meta(out, meta, ""),
        _ => print_tree(out, store, "", "", &group_rows(kind), depth),
    }
}

/// Print the children of the node at `path`, one line each, indented by
/// `prefix`.
///
/// `store` is where the metadata comes from, and the only thing here that
/// knows whether these nodes are directories or S3 prefixes. `path` names the
/// node inside it -- see `Store`.
///
/// `rows` is the node's own metadata, one finished line each, in the order they
/// should appear. They are printed here rather than by the caller because this
/// is the one place that already knows whether any children follow them, which
/// is what decides the last connector. An empty slice means there is nothing to
/// print above the children.
///
/// `out` is where the lines go. It was already returning `io::Result` for the
/// directory reads; writes now join them, so a closed pipe stops the walk the
/// same way an unreadable directory does.
///
/// `depth` is how many more levels below this node may be listed, or `None` for
/// all of them. `Some(0)` means this node is as deep as the output goes: its
/// own metadata rows still print, because those describe the node itself
/// rather than anything below it.
fn print_tree(
    out: &mut dyn Write,
    store: &dyn Store,
    path: &str,
    prefix: &str,
    rows: &[String],
    depth: Option<usize>,
) -> io::Result<()> {
    let children = child_dirs(store, path, depth)?;

    // This directory's own metadata, above its children. Metadata rows keep
    // the shorter two-dash stem that tells them apart from node rows.
    for (i, row) in rows.iter().enumerate() {
        // `└─` closes a branch, so it belongs to the last row only when there
        // are no children below it to keep the branch open.
        let is_last = i == rows.len() - 1 && children.is_empty();
        let connector = if is_last { "└─ " } else { "├─ " };
        writeln!(out, "{prefix}{connector}{row}")?;
    }

    for (i, name) in children.iter().enumerate() {
        let is_last = i == children.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child = child_path(path, name);
        let kind = classify(store, &child);
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
                store,
                &child,
                &child_prefix,
                &group_rows(&kind),
                depth.map(|depth| depth - 1),
            )?,
        }
    }

    Ok(())
}

/// The children of the node at `path`, sorted, and honouring the depth limit.
///
/// Collected rather than streamed because the tree needs to know which child is
/// last before it can draw a connector, and neither `read_dir` nor an S3
/// listing promises an order.
///
/// `Some(0)` means this node is as deep as the output goes, and the store is
/// then not asked at all. That is not only cheaper on a store full of chunk
/// files -- remotely it is one HTTP request saved per node -- but an empty list
/// is also exactly what a node with no children has, so neither renderer needs
/// a rule of its own for the limit.
///
/// Both renderers call this, which is what keeps them agreeing about which
/// children exist and in what order.
fn child_dirs(store: &dyn Store, path: &str, depth: Option<usize>) -> io::Result<Vec<String>> {
    if depth == Some(0) {
        return Ok(Vec::new());
    }

    store.children(path)
}

/// Decide what kind of Zarr node sits at `path` by reading the metadata files
/// directly inside it.
///
/// Classification never fails: a node we cannot read, or whose metadata we do
/// not understand, is simply `Unknown`. A broken `zarr.json` in one corner of
/// the tree should not abort the whole walk.
///
/// Three reads at worst, and they are in the order they are for a reason.
/// Every one of them is an HTTP GET on a remote store, which is what decided
/// the shape of everything below: a file is read once and its text kept, and
/// nothing is opened twice.
fn classify(store: &dyn Store, path: &str) -> NodeKind {
    // Zarr V2 keeps the two node kinds in separate files, so which file is
    // there answers the question. Locally that used to be a stat and is now a
    // read of a very small file; remotely there is no cheaper way to ask.
    if store.read(&child_path(path, ".zgroup")).is_some() {
        // V2 keeps user attributes in a file of their own. It is read once
        // here and handed to both readers below, which read different keys out
        // of it and know nothing about each other -- but which would otherwise
        // cost a second request for a file already in hand.
        let attrs = read_json(store, &child_path(path, ".zattrs"));
        return NodeKind::Group(GroupMeta {
            ome: attrs.as_ref().and_then(ome_info_v2),
            spatialdata: attrs.as_ref().and_then(spatialdata_info_v2),
        });
    }

    if let Some(zarray) = store.read(&child_path(path, ".zarray")) {
        return NodeKind::Array(array_meta_v2(&zarray));
    }

    // Zarr V3 uses one filename for both kinds and moves the distinction inside
    // the file, so here we do have to look inside it. Checked second, so a
    // store that carries both V2 and V3 metadata is reported as V2.
    if let Some(kind) = classify_v3(store, &child_path(path, "zarr.json")) {
        return kind;
    }

    NodeKind::Unknown
}

/// Read `node_type` out of a Zarr V3 `zarr.json`, or `None` if the file is
/// missing, unreadable, not valid JSON, or has no recognisable `node_type`.
fn classify_v3(store: &dyn Store, path: &str) -> Option<NodeKind> {
    let value = read_json(store, path)?;

    match value.get("node_type")?.as_str()? {
        "group" => Some(NodeKind::Group(GroupMeta {
            ome: ome_info_v3(&value),
            spatialdata: spatialdata_info_v3(&value),
        })),
        "array" => Some(NodeKind::Array(array_meta_v3(&value))),
        _ => None,
    }
}

/// Look for OME-Zarr image metadata in an already-parsed Zarr V2 `.zattrs`.
///
/// V2 keeps user attributes in a file separate from `.zgroup`, and that file
/// *is* the attributes object -- so `attrs` is its whole contents. A `.zattrs`
/// that was missing or unparseable never reaches here: `classify` has nothing
/// to hand over, and the group simply gets no tag.
fn ome_info_v2(attrs: &Value) -> Option<OmeInfo> {
    // V2 predates the `ome` namespace: the keys sit at the top level of
    // `.zattrs`, and the version belongs to an individual multiscale rather
    // than to the group. Real 0.4 stores often leave it out entirely.
    let version = attrs
        .get("multiscales")
        .and_then(|value| value.as_array())
        .and_then(|entries| entries.first())
        .and_then(|first| first.get("version"));

    ome_info(attrs, version)
}

/// Look for OME-Zarr image metadata in an already-parsed Zarr V3 `zarr.json`.
///
/// V3 keeps group attributes in that same file, so nothing extra is read here.
/// The OME-Zarr keys live under a namespace of their own, version included.
fn ome_info_v3(value: &Value) -> Option<OmeInfo> {
    let ome = value.get("attributes")?.get("ome")?;
    ome_info(ome, ome.get("version"))
}

/// Collect what we display about an OME-Zarr group, or `None` if this is an
/// ordinary Zarr group.
///
/// `ome` is whichever object holds the OME-Zarr keys -- the whole `.zattrs` for
/// V2, `attributes.ome` for V3 -- and `version` is passed in separately because
/// that is the only other thing the two layouts disagree about.
///
/// The three kinds are tried in turn, and the first key found decides. They do
/// not overlap in practice: a group is an image, a plate or a well, and the
/// metadata says which by which key it wrote.
fn ome_info(ome: &Value, version: Option<&Value>) -> Option<OmeInfo> {
    // Kept exactly as stored, and never checked against the versions we happen
    // to know about: this tool reports metadata rather than validating it. A
    // version that is absent, or is not a string, just leaves the tag bare.
    let namespace_version =
        |value: Option<&Value>| value.and_then(|value| value.as_str()).map(String::from);

    // A `multiscales` key holding a non-empty array is what makes a group an
    // image. Missing, the wrong JSON type, or empty all mean it is not one.
    if let Some(first) = ome
        .get("multiscales")
        .and_then(|value| value.as_array())
        .and_then(|entries| entries.first())
    {
        // Axes belong to an individual multiscale rather than to the group, so
        // they come from that same first entry in both layouts -- the one part
        // of this metadata V2 and V3 already agree about.
        //
        // `datasets` is the one part of this metadata that has not changed
        // shape since OME-NGFF 0.1 -- always a list of objects with a `path` --
        // so unlike the axes it needs no per-version handling at all. What 0.4
        // added to each entry, `coordinateTransformations`, is not read.
        return Some(OmeInfo {
            kind: OmeKind::Image,
            version: namespace_version(version),
            axes: axis_names(first.get("axes")),
            datasets: dataset_paths(first.get("datasets")),
        });
    }

    // Where the version comes from differs again for the HCS keys. V3 records
    // it once for the whole `ome` namespace, but V2 has no namespace to record
    // it in and puts it inside the `plate` or `well` object instead -- the same
    // split `multiscales` has, in a different place. The object's own version
    // wins where there is one, and the namespace answers otherwise.
    let stored_version = |object: &Value| {
        namespace_version(object.get("version")).or_else(|| namespace_version(version))
    };

    if let Some(plate) = ome.get("plate").filter(|plate| plate.is_object()) {
        // Each count is the length of a declared list, and is missing on its
        // own when that list is. Nothing is counted from the directories: a
        // plate declaring 96 wells says 96 whether or not 96 were written.
        let declared = |key: &str| {
            plate
                .get(key)
                .and_then(|value| value.as_array())
                .map(|entries| entries.len())
        };

        return Some(OmeInfo {
            kind: OmeKind::Plate {
                rows: declared("rows"),
                columns: declared("columns"),
                wells: declared("wells"),
            },
            version: stored_version(plate),
            axes: None,
            datasets: None,
        });
    }

    if let Some(well) = ome.get("well").filter(|well| well.is_object()) {
        return Some(OmeInfo {
            kind: OmeKind::Well,
            version: stored_version(well),
            axes: None,
            datasets: None,
        });
    }

    None
}

/// Look for SpatialData metadata in an already-parsed Zarr V2 `.zattrs`.
///
/// V2 keeps user attributes in a file of their own, and that file *is* the
/// attributes object -- so the markers sit at its top level, rather than under
/// an `attributes` field the way V3 nests it.
///
/// `attrs` is the same value `ome_info_v2` was given: `classify` reads the
/// file once and hands it to both. They read different keys and know nothing
/// about each other, which is why they are still two functions.
fn spatialdata_info_v2(attrs: &Value) -> Option<SpatialData> {
    // V2 predates the `ome` namespace, so the OME-Zarr keys sit at the top
    // level of `.zattrs` alongside SpatialData's own -- the two objects this
    // function has to tell apart are, here, one and the same.
    spatialdata_info(attrs, Some(attrs))
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
    // A plate says how big it declares itself to be. Each count is independent,
    // so a plate that declared only some of the three lists shows only those.
    // A well adds no rows at all: its images are the child groups below it, and
    // the tree is already printing them.
    if let OmeKind::Plate {
        rows: plate_rows,
        columns,
        wells,
    } = &ome.kind
    {
        if let Some(count) = plate_rows {
            rows.push(format!("rows: {count}"));
        }
        if let Some(count) = columns {
            rows.push(format!("columns: {count}"));
        }
        if let Some(count) = wells {
            rows.push(format!("wells: {count}"));
        }
    }
    rows
}

/// Read a metadata file and parse it as JSON, or `None` if either step fails.
///
/// Missing, unreadable and unparseable all come back the same way, because
/// every caller does the same thing with them. `?` returns None early on the
/// read, and `.ok()` drops the parse error: we care *that* this failed, not
/// why.
fn read_json(store: &dyn Store, path: &str) -> Option<Value> {
    serde_json::from_str(&store.read(path)?).ok()
}

/// Collect the display metadata from the text of a Zarr V2 `.zarray`.
///
/// Takes the text rather than a parsed value, and that is the whole point: the
/// filename has already told us this is an array, so text we cannot parse
/// costs us only the metadata. Every field comes back missing and the node is
/// still shown as an array.
fn array_meta_v2(text: &str) -> ArrayMeta {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
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
fn json_tree(
    store: &dyn Store,
    path: &str,
    name: &str,
    kind: NodeKind,
    depth: Option<usize>,
) -> io::Result<Value> {
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
        for name in child_dirs(store, path, depth)? {
            let child = child_path(path, &name);
            // Classified here rather than inside the call, so that the root --
            // which `run` had to classify before it could check the store --
            // is not classified a second time.
            let child_kind = classify(store, &child);
            children.push(json_tree(
                store,
                &child,
                &name,
                child_kind,
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
///
/// `kind` names which of the three OME-Zarr groups this is, so a reader can
/// tell them apart without unpicking the tag. A plate's counts follow the same
/// rule the tree's rows follow, and the same one `shards` follows on an array:
/// a count that was not declared is left out rather than written as `null`,
/// because only a plate has these to declare in the first place.
fn json_ome(ome: &OmeInfo) -> Value {
    let mut value = json!({
        "tag": ome.tag(),
        "kind": ome.kind.name(),
        "version": ome.version,
        "axes": ome.axes,
        "pyramid_levels": ome.datasets.as_ref().map(|datasets| datasets.len()),
        "datasets": ome.datasets,
    });

    if let OmeKind::Plate {
        rows,
        columns,
        wells,
    } = &ome.kind
    {
        if let Some(count) = rows {
            value["rows"] = json!(count);
        }
        if let Some(count) = columns {
            value["columns"] = json!(count);
        }
        if let Some(count) = wells {
            value["wells"] = json!(count);
        }
    }

    value
}

#[cfg(test)]
mod tests {
    // `tests` is a child module of the crate root, so it can already see the
    // root's private items. This glob import just brings their names into
    // scope so we can call them unqualified.
    use super::*;
    use object_store::PutPayload;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;

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

    /// An empty fixture directory inside the system temp directory, named for
    /// the test that asked for it. The process id keeps two simultaneous test
    /// runs from colliding, and the name keeps two tests in one run from
    /// deleting each other's files.
    fn fixture(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("zarr-tree-test-{name}-{}", process::id()));
        // Left over from an earlier run that panicked before its cleanup.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Classify a directory of real files the way the walk does: through a
    /// `LocalStore`, whose root is the directory itself.
    ///
    /// The V2 readers take a parsed `.zattrs`, which `classify` reads for
    /// them, so a test about V2's *file layout* has to come in this way round.
    /// It gets the whole path exercised for its trouble.
    fn classify_dir(dir: &Path) -> NodeKind {
        classify(&LocalStore::new(&dir.to_string_lossy()), "")
    }

    /// The same, for the tests that go on to ask what the group's attributes
    /// said about it.
    fn group_meta(dir: &Path) -> GroupMeta {
        match classify_dir(dir) {
            NodeKind::Group(meta) => meta,
            _ => panic!("a `.zgroup` makes this a group"),
        }
    }

    /// One small Zarr V3 store, as the metadata objects that make it up.
    ///
    /// Written into a directory by `write_fixture` and into an in-memory
    /// object store by `memory_store`, so that the same store can be walked
    /// both ways and the two outputs compared.
    ///
    /// The last two entries are chunk objects. They are the point of the
    /// fixture as much as the metadata is: nothing in either output may ever
    /// name them.
    const FIXTURE: &[(&str, &str)] = &[
        (
            "zarr.json",
            r#"{"zarr_format": 3, "node_type": "group", "attributes": {"ome": {
                "version": "0.5",
                "multiscales": [{
                    "axes": [{"name": "y"}, {"name": "x"}],
                    "datasets": [{"path": "0"}]
                }]
            }}}"#,
        ),
        (
            "0/zarr.json",
            r#"{"zarr_format": 3, "node_type": "array",
                "shape": [64, 64], "data_type": "uint16",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [32, 32]}},
                "codecs": [{"name": "sharding_indexed",
                            "configuration": {"chunk_shape": [16, 16]}}]}"#,
        ),
        ("0/c/0/0", "chunk, not metadata"),
        ("0/c/0/1", "chunk, not metadata"),
    ];

    /// A store four levels deep, for the tests about `--depth`.
    const NESTED: &[(&str, &str)] = &[
        ("zarr.json", r#"{"zarr_format": 3, "node_type": "group"}"#),
        ("A/zarr.json", r#"{"zarr_format": 3, "node_type": "group"}"#),
        (
            "A/1/zarr.json",
            r#"{"zarr_format": 3, "node_type": "group"}"#,
        ),
        (
            "A/1/0/zarr.json",
            r#"{"zarr_format": 3, "node_type": "array", "shape": [4], "data_type": "uint8",
                "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [4]}}}"#,
        ),
    ];

    /// Write a fixture into a directory, creating the directories it implies.
    fn write_fixture(dir: &Path, objects: &[(&str, &str)]) {
        for (key, body) in objects {
            let path = dir.join(key);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
    }

    /// A `RemoteStore` over `object_store`'s in-memory store, holding a
    /// fixture written under `prefix`.
    ///
    /// `InMemory` is a real `ObjectStore`: it answers `get` and
    /// `list_with_delimiter` as S3 does, common prefixes and all. That is what
    /// lets the remote walk be tested here with no network, no mock server and
    /// no AWS account -- and what is under test is the same `RemoteStore` that
    /// an `s3://` URI builds, differing only in which object store it wraps.
    fn memory_store(uri: &str, prefix: &str, objects: &[(&str, &str)]) -> RemoteStore {
        let store = RemoteStore::new(
            object_store::memory::InMemory::new(),
            prefix,
            uri,
            Backend::S3,
        )
        .unwrap();

        for (key, body) in objects {
            store
                .runtime
                .block_on(store.store.put(
                    &ObjectPath::from(child_path(prefix, key)),
                    PutPayload::from(String::from(*body)),
                ))
                .unwrap();
        }

        store
    }

    /// The tree a store draws, as the text a person would see.
    fn rendered(store: &dyn Store, name: &str, depth: Option<usize>) -> String {
        let kind = classify(store, "");
        let mut out: Vec<u8> = Vec::new();
        print_store(&mut out, store, name, &kind, depth).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// A minimal HTTP server, on a thread of its own.
    ///
    /// It answers the three things zarr-tree ever asks of a server and nothing
    /// else: `GET` and `HEAD` for metadata, and -- when `webdav` is set --
    /// WebDAV `PROPFIND` with `Depth: 1` for children. With `webdav` off it
    /// refuses `PROPFIND` the way an ordinary static file server does, which
    /// is the whole reason it is worth having one.
    ///
    /// One request per connection, no keep-alive, no concurrency. Enough to
    /// prove the URL mapping and the listing behaviour against the real
    /// `object_store` HTTP client; not a web server.
    struct TestServer {
        /// The URL of the store root, as a person would type it.
        url: String,
        /// Every request path the server has seen, in order. What proves a
        /// chunk was never asked for is its absence from here.
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl TestServer {
        fn start(root: &str, objects: &[(&str, &str)], webdav: bool) -> TestServer {
            // Port 0 asks the operating system for a free one, so tests running
            // side by side cannot collide.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();

            let files: BTreeMap<String, String> = objects
                .iter()
                .map(|(key, body)| (child_path(root, key), String::from(*body)))
                .collect();

            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&requests);

            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let Some((method, target)) = read_request(&mut stream) else {
                        continue;
                    };
                    log.lock().unwrap().push(target.clone());

                    // Keys are stored without the leading slash the URL has.
                    let key = target.split(['?', '#']).next().unwrap_or("");
                    let key = key.trim_matches('/');

                    let response = match method.as_str() {
                        "GET" | "HEAD" => match files.get(key) {
                            Some(body) => http_response("200 OK", body, method == "HEAD"),
                            None => http_response("404 Not Found", "", method == "HEAD"),
                        },
                        "PROPFIND" if webdav => propfind(&files, key),
                        // What a static server says. `object_store` hands this
                        // back as a generic failure, which is what
                        // `RemoteStore::diagnose` has to make sense of.
                        _ => http_response("405 Method Not Allowed", "", false),
                    };

                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });

            TestServer {
                url: format!("http://127.0.0.1:{port}/{root}"),
                requests,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        /// The store this server's root URL opens, through the same
        /// `open_store` the command line goes through.
        fn open(&self) -> Box<dyn Store> {
            open_store(&self.url).unwrap()
        }
    }

    /// The method and target of one request, or `None` if the connection died.
    fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).ok()? == 0 {
                return None;
            }
            head.push(byte[0]);
        }

        // "PROPFIND /data/store.zarr HTTP/1.1" -- the first two words are all
        // that is needed. `Depth` is not read: `list_with_delimiter` is the
        // only caller and it always asks for 1.
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut words = text.split_whitespace();
        Some((String::from(words.next()?), String::from(words.next()?)))
    }

    fn http_response(status: &str, body: &str, head_only: bool) -> String {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/xml\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        match head_only {
            true => head,
            false => format!("{head}{body}"),
        }
    }

    /// A WebDAV `Depth: 1` answer: the collection itself, then one entry per
    /// immediate child.
    ///
    /// `object_store` drops the collection itself by path length and turns the
    /// rest into common prefixes and objects, so what this has to get right is
    /// the shape, not the filtering.
    fn propfind(files: &BTreeMap<String, String>, prefix: &str) -> String {
        let inside = format!("{prefix}/");
        if !files.keys().any(|key| key.starts_with(&inside)) {
            // No such collection. A WebDAV server answers 404, and
            // `object_store` reads that as "no children" rather than an error.
            return http_response("404 Not Found", "", false);
        }

        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<multistatus xmlns=\"DAV:\">\n",
        );
        body.push_str(&dav_entry(&format!("/{inside}"), None));

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (key, text) in files {
            let Some(rest) = key.strip_prefix(&inside) else {
                continue;
            };
            match rest.split_once('/') {
                // A child collection, named once however many keys sit below.
                Some((directory, _)) => {
                    if seen.insert(directory) {
                        body.push_str(&dav_entry(&format!("/{inside}{directory}/"), None));
                    }
                }
                None => body.push_str(&dav_entry(&format!("/{inside}{rest}"), Some(text.len()))),
            }
        }

        body.push_str("</multistatus>\n");
        http_response("207 Multi-Status", &body, false)
    }

    /// One `<response>` element. `size` is `None` for a collection.
    ///
    /// The date format is the one `object_store` insists on -- RFC 1123, which
    /// it parses as `%a, %d %h %Y %T GMT` -- and a missing `getlastmodified`
    /// fails the whole listing, so it is not optional here even though nothing
    /// reads it. chrono checks the weekday against the date, so the two have
    /// to agree: 1 January 1970 really was a Thursday.
    fn dav_entry(href: &str, size: Option<usize>) -> String {
        let (resource_type, length) = match size {
            Some(size) => (
                String::new(),
                format!("<getcontentlength>{size}</getcontentlength>"),
            ),
            None => (String::from("<collection/>"), String::new()),
        };

        format!(
            "<response><href>{href}</href><propstat><prop>\
             <getlastmodified>Thu, 01 Jan 1970 00:00:00 GMT</getlastmodified>\
             {length}<resourcetype>{resource_type}</resourcetype>\
             </prop><status>HTTP/1.1 200 OK</status></propstat></response>\n"
        )
    }

    #[test]
    fn an_http_uri_is_split_into_a_base_and_a_path() {
        // The base is the origin and nothing more, because a WebDAV listing
        // answers with paths from the server root. The path becomes the key
        // prefix, exactly as an S3 key does.
        let Ok(Location::Http { base, path }) =
            parse_location("https://server.example/data/store.zarr")
        else {
            panic!("an https:// URI names a server");
        };
        assert_eq!(base, "https://server.example/");
        assert_eq!(path, "data/store.zarr");

        // Cleartext, a port, and a trailing slash.
        let Ok(Location::Http { base, path }) = parse_location("http://127.0.0.1:8080/store.zarr/")
        else {
            panic!("an http:// URI names a server");
        };
        assert_eq!(base, "http://127.0.0.1:8080/");
        assert_eq!(path, "store.zarr");

        // A percent-encoded path is decoded here, because the client encodes
        // each segment again on its way out. Left encoded it would go out as
        // `my%2520data`.
        let Ok(Location::Http { path, .. }) =
            parse_location("https://server.example/my%20data/s.zarr")
        else {
            panic!("an escaped path is still a path");
        };
        assert_eq!(path, "my data/s.zarr");

        // A query string rides on the base, so every request carries it --
        // which is the one shape of access token a static server tends to
        // want. A fragment never reaches a server and is dropped.
        let Ok(Location::Http { base, path }) =
            parse_location("https://server.example/s.zarr?token=abc#top")
        else {
            panic!("a query string is still a URI");
        };
        assert_eq!(base, "https://server.example/?token=abc");
        assert_eq!(path, "s.zarr");

        // And the scheme is the only thing that decides. A local path that
        // merely contains one somewhere after its start is a local path.
        for local in ["store.zarr", "/data/store.zarr", "./mirrors/https://x"] {
            assert!(
                matches!(parse_location(local), Ok(Location::Local)),
                "{local:?} names a directory on this machine"
            );
        }
    }

    #[test]
    fn a_malformed_http_uri_is_rejected() {
        // Nothing to name a server with. Rejected rather than read as a
        // relative path, because the scheme said what was meant -- the same
        // rule `s3://` follows.
        for broken in ["http://", "https://", "http://?token=x", "https://[bad"] {
            let Err(error) = parse_location(broken) else {
                panic!("{broken:?} names no server");
            };
            assert!(error.contains("invalid url"), "{error}");
        }

        // Not malformed, though it looks it: URL parsing strips the extra
        // slashes off an authority, so this names the host `store.zarr` and
        // not a path on some unnamed server. Asserted so that nobody
        // "corrects" it into the list above.
        let Ok(Location::Http { base, path }) = parse_location("https:///store.zarr") else {
            panic!("the extra slash is stripped, leaving a host");
        };
        assert_eq!(base, "https://store.zarr/");
        assert_eq!(path, "");
    }

    #[test]
    fn the_same_store_reads_the_same_over_http_as_on_disk() {
        // The same assertion the S3 tests make, over a real HTTP client and a
        // real WebDAV listing. Everything in this tree -- the OME-Zarr 0.5
        // tag, the axes, the pyramid, the sharded array's two grids -- is read
        // by functions that never learn where the bytes came from.
        let dir = fixture("http-neutral");
        write_fixture(&dir, FIXTURE);
        let local = LocalStore::new(&dir.to_string_lossy());
        let local_tree = rendered(&local, "store.zarr", None);
        fs::remove_dir_all(&dir).unwrap();

        let server = TestServer::start("data/store.zarr", FIXTURE, true);
        let remote = server.open();
        let remote_tree = rendered(remote.as_ref(), "store.zarr", None);

        assert_eq!(local_tree, remote_tree);
        assert!(local_tree.contains("store.zarr [group, OME-Zarr 0.5]"));
        assert!(local_tree.contains("chunks: [16, 16]"));
        assert!(local_tree.contains("shards: [32, 32]"));

        // The URL mapping, checked from the server's side: every request
        // landed under the store root, with no doubled or missing separator.
        for request in server.requests() {
            assert!(
                request.starts_with("/data/store.zarr"),
                "{request:?} is not under the store root"
            );
            assert!(!request.contains("//"), "{request:?} has a doubled slash");
        }
    }

    #[test]
    fn an_http_array_is_a_leaf_and_its_chunks_are_never_requested() {
        let server = TestServer::start("data/store.zarr", FIXTURE, true);
        let store = server.open();
        let tree = rendered(store.as_ref(), server.url.as_str(), None);

        assert!(tree.contains("0 [array]"), "the array itself is shown");
        assert!(!tree.contains("── c"), "the chunk directory is not a child");

        // Nothing under the array was asked for -- neither its chunk objects
        // nor a listing of the prefix holding them.
        for request in server.requests() {
            assert!(
                !request.contains("/0/c"),
                "{request:?} reaches into chunk storage"
            );
        }

        // And not because there was nothing to find: asked directly, the
        // server hands back the chunk prefix the walk declined to ask for.
        assert_eq!(store.children("0").unwrap(), vec![String::from("c")]);
    }

    #[test]
    fn depth_limits_an_http_walk_the_way_it_limits_a_local_one() {
        let server = TestServer::start("srv/store.zarr", NESTED, true);
        let store = server.open();

        // Depth 0 is the root alone, and costs no listing at all -- the same
        // saving it makes on S3.
        assert_eq!(
            rendered(store.as_ref(), "store.zarr", Some(0)),
            "store.zarr [group]\n"
        );
        assert!(
            !server.requests().iter().any(|request| request.is_empty()),
            "no listing should have been made"
        );

        let one = rendered(store.as_ref(), "store.zarr", Some(1));
        assert!(one.contains("── A [group]"));
        assert!(!one.contains("── 1 [group]"), "{one}");

        let two = rendered(store.as_ref(), "store.zarr", Some(2));
        assert!(two.contains("── 1 [group]"));
        assert!(!two.contains("── 0 [array]"), "{two}");

        let all = rendered(store.as_ref(), "store.zarr", None);
        assert!(all.contains("── 0 [array]"), "{all}");
    }

    #[test]
    fn a_server_that_cannot_list_says_so_rather_than_saying_the_store_is_missing() {
        // An ordinary static file server: `GET` works, `PROPFIND` does not.
        let server = TestServer::start("data/store.zarr", FIXTURE, false);
        let store = server.open();

        // The metadata reads succeed, so the root is identified and the root
        // check passes without a listing -- exactly as it would on S3.
        let kind = classify(store.as_ref(), "");
        assert!(matches!(kind, NodeKind::Group(_)), "the root is readable");
        assert!(store.check_root(true).is_ok());

        // Only then does the listing fail, and what it says is that the server
        // cannot list. Saying the store was not found would be plainly wrong:
        // the group tag above was read from that very URL.
        let error = store
            .children("")
            .expect_err("this server does not implement PROPFIND");
        let message = error.to_string();
        assert!(message.contains("cannot list"), "{message}");
        assert!(message.contains("WebDAV"), "{message}");
        assert!(
            !message.contains("not found") && !message.contains("does not exist"),
            "a readable root must not be reported as missing: {message}"
        );
    }

    #[test]
    fn a_path_is_local_unless_it_names_a_bucket() {
        // Every spelling a local path has ever been given here, and one that
        // carries the scheme somewhere other than the front -- a directory may
        // legitimately be called that, and nothing but a leading `s3://` makes
        // an argument remote.
        for local in [
            "store.zarr",
            "/data/store.zarr",
            "./store.zarr",
            "../data/store.zarr",
            "backups/s3://not-a-uri",
        ] {
            assert!(
                matches!(parse_location(local), Ok(Location::Local)),
                "{local:?} names a directory on this machine"
            );
        }

        let Ok(Location::S3 { bucket, key }) = parse_location("s3://bucket/path/store.zarr") else {
            panic!("an s3:// URI names a bucket");
        };
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "path/store.zarr");

        // A bucket and nothing more: the whole bucket is the store, and the
        // prefix is empty rather than missing.
        let Ok(Location::S3 { bucket, key }) = parse_location("s3://bucket") else {
            panic!("a bucket on its own is a store");
        };
        assert_eq!(bucket, "bucket");
        assert_eq!(key, "");

        // A trailing slash is how a person writes a prefix, and names the same
        // place without one.
        let Ok(Location::S3 { key, .. }) = parse_location("s3://bucket/store.zarr/") else {
            panic!("a trailing slash is still a URI");
        };
        assert_eq!(key, "store.zarr");

        // Nothing to name a bucket with. Rejected rather than read as a
        // relative path, because the scheme said what was meant.
        for broken in ["s3://", "s3:///store.zarr"] {
            assert!(
                parse_location(broken).is_err(),
                "{broken:?} names no bucket"
            );
        }
    }

    #[test]
    fn the_same_store_reads_the_same_whether_it_is_local_or_remote() {
        // The whole point of the `Store` trait, in one assertion. Every fact
        // in this tree -- that the root is an OME-Zarr 0.5 image, its axes,
        // its pyramid, that `0` is a sharded array and what its two grids are
        // -- is read by functions that never learn where the bytes came from.
        let dir = fixture("neutral");
        write_fixture(&dir, FIXTURE);
        let local = LocalStore::new(&dir.to_string_lossy());
        let local_tree = rendered(&local, "store.zarr", None);
        fs::remove_dir_all(&dir).unwrap();

        let remote = memory_store("s3://bucket/store.zarr", "store.zarr", FIXTURE);
        let remote_tree = rendered(&remote, "store.zarr", None);

        assert_eq!(local_tree, remote_tree);

        // And it is the tree we meant, not two identically empty ones.
        assert!(local_tree.contains("store.zarr [group, OME-Zarr 0.5]"));
        assert!(local_tree.contains("axes: y, x"));
        assert!(local_tree.contains("0 [array]"));
        assert!(local_tree.contains("chunks: [16, 16]"));
        assert!(local_tree.contains("shards: [32, 32]"));
    }

    #[test]
    fn a_remote_array_is_a_leaf_and_its_chunks_are_never_listed() {
        // This is the rule that makes remote traversal usable at all. A real
        // OME-Zarr array on S3 has millions of chunk objects under it, and
        // listing even one prefix of them would cost thousands of requests.
        let store = memory_store("s3://bucket/store.zarr", "store.zarr", FIXTURE);
        let tree = rendered(&store, "s3://bucket/store.zarr", None);

        assert!(tree.contains("0 [array]"), "the array itself is shown");
        assert!(
            !tree.contains("── c"),
            "the V3 chunk directory is not a child:\n{tree}"
        );
        assert!(
            !tree.contains("[unknown]"),
            "nothing beneath the array is reached at all:\n{tree}"
        );

        // And not because there was nothing to find. Asked directly, the store
        // hands back the chunk prefix that the walk declined to ask for --
        // which is what distinguishes pruning from an empty bucket.
        assert_eq!(store.children("0").unwrap(), vec![String::from("c")]);
    }

    #[test]
    fn depth_limits_a_remote_walk_the_way_it_limits_a_local_one() {
        let store = memory_store("s3://bucket/store.zarr", "store.zarr", NESTED);

        // The root on its own. Nothing below it is listed, which remotely means
        // no listing request is made at all.
        let root_only = rendered(&store, "store.zarr", Some(0));
        assert_eq!(root_only, "store.zarr [group]\n");

        // One level, then two. `A/1/0` is the array at the bottom, and it
        // appears only once the limit reaches it.
        let one = rendered(&store, "store.zarr", Some(1));
        assert!(one.contains("── A [group]"));
        assert!(!one.contains("── 1 [group]"), "{one}");

        let two = rendered(&store, "store.zarr", Some(2));
        assert!(two.contains("── 1 [group]"));
        assert!(!two.contains("── 0 [array]"), "{two}");

        // And unlimited reaches the array at the bottom.
        let all = rendered(&store, "store.zarr", None);
        assert!(all.contains("── 0 [array]"), "{all}");
    }

    #[test]
    fn a_remote_store_reports_the_same_json_as_a_local_one() {
        // `--json` is a second renderer over the same walk, so it has the same
        // claim to be storage-neutral -- and the same chunk objects to leave
        // out.
        let dir = fixture("neutral-json");
        write_fixture(&dir, FIXTURE);
        let local = LocalStore::new(&dir.to_string_lossy());
        let local_json =
            json_tree(&local, "", "store.zarr", classify(&local, ""), Some(2)).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        let remote = memory_store("s3://bucket/store.zarr", "store.zarr", FIXTURE);
        let remote_json =
            json_tree(&remote, "", "store.zarr", classify(&remote, ""), Some(2)).unwrap();

        assert_eq!(local_json, remote_json);

        // The array is a leaf here too: `children` is present and empty, and
        // no chunk key appears anywhere in the document.
        assert_eq!(remote_json["children"][0]["kind"], json!("array"));
        assert_eq!(remote_json["children"][0]["children"], json!([]));
    }

    #[test]
    fn a_remote_root_that_is_not_there_is_an_error() {
        // A prefix with no metadata and no children is a mistyped URI, and
        // saying so beats printing an empty tree. The local walk has said as
        // much since the beginning, from `exists()`; remotely it takes a
        // listing, and only when nothing identified the root.
        let store = memory_store("s3://bucket/store.zarr", "store.zarr", FIXTURE);
        assert!(store.check_root(false).is_ok(), "that prefix is there");

        // The same bucket, seen from a prefix nothing was written under: a
        // mistyped URI rather than a missing bucket, and from one listing
        // there is no telling the two apart.
        let mistyped = RemoteStore {
            prefix: ObjectPath::from("nope.zarr"),
            uri: String::from("s3://bucket/nope.zarr"),
            ..store
        };
        let error = mistyped
            .check_root(false)
            .expect_err("nothing was written under that prefix");
        assert!(
            error.to_string().contains("no such bucket or prefix"),
            "{error}"
        );

        // And a root the metadata already identified is not listed for at all.
        // Nothing here can tell that from the outside, which is the point: the
        // listing that would have failed is never made.
        assert!(mistyped.check_root(true).is_ok());
    }

    #[test]
    fn anonymous_access_is_the_default_only_when_nothing_names_a_credential() {
        // Reading this environment rather than a fixture, so the assertion has
        // to be phrased against whatever the test runner happens to have set.
        // The rule itself is simple enough to state either way round.
        let named = [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        ]
        .iter()
        .any(|name| env::var_os(name).is_some());
        let overridden = env::var_os("AWS_SKIP_SIGNATURE").is_some();

        assert_eq!(anonymous_by_default(), !named && !overridden);
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
            (vec!["one.zarr", "two.zarr"], "exactly one store"),
            (vec![], "expected a store"),
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
        let dir = fixture("v2");
        fs::write(
            dir.join(".zarray"),
            r#"{"shape": [4096, 4096], "chunks": [512, 512], "dtype": "<u2"}"#,
        )
        .unwrap();

        // Through the store, so that this covers the whole V2 array path: the
        // filename that makes it an array, and the reading of the file.
        let NodeKind::Array(meta) = classify_dir(&dir) else {
            panic!("a `.zarray` makes this an array");
        };

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
        let dir = fixture("v3-malformed");

        // Truncated mid-object: serde_json will refuse to parse this.
        fs::write(dir.join("zarr.json"), r#"{"zarr_format": 3, "node_type":"#).unwrap();

        let kind = classify_dir(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // One corrupt file should cost us only that node's label, not the
        // walk. `matches!` checks the variant without making NodeKind derive
        // PartialEq and Debug that nothing outside this test would use.
        assert!(matches!(kind, NodeKind::Unknown));
    }

    #[test]
    fn v2_dtype_is_passed_through_uninterpreted() {
        // `<M8[ns]` is NumPy's datetime64, valid V2 but outside what a Zarr
        // library would necessarily load. We only display dtypes, so it has
        // to survive to the screen unchanged.
        let meta = array_meta_v2(
            r#"{"zarr_format": 2, "shape": [10], "chunks": [10], "dtype": "<M8[ns]"}"#,
        );

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
        // An image is still a bare "OME-Zarr 0.5" with no kind appended, which
        // is what leaves every existing image label untouched by HCS.
        assert_eq!(info.kind.name(), "image");
        assert_eq!(joined(&info.axes), Some(String::from("c, y, x")));
        assert_eq!(info.datasets, Some(vec![String::from("0")]));
    }

    #[test]
    fn ome_v2_multiscales_without_version_is_still_detected() {
        let dir = fixture("ome-v2");

        // A V2 image group, written the way real 0.4 stores often are: the keys
        // at the top level of `.zattrs`, and no `version` anywhere. The group is
        // still an OME-Zarr image, so it earns a tag -- just a bare one.
        fs::write(dir.join(".zgroup"), r#"{"zarr_format": 2}"#).unwrap();
        fs::write(
            dir.join(".zattrs"),
            r#"{"multiscales": [{"datasets": [{"path": "0"}]}]}"#,
        )
        .unwrap();

        let info = group_meta(&dir).ome;

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
    fn a_plate_is_recognised_and_reports_its_declared_counts() {
        // A plate as ome-zarr-py writes one: an `ome.plate` object holding the
        // three lists, and no `multiscales` anywhere.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "plate": {
                        "version": "0.5",
                        "rows": [{ "name": "A" }, { "name": "B" }],
                        "columns": [{ "name": "1" }, { "name": "2" }, { "name": "3" }],
                        "wells": [
                            { "path": "A/1", "rowIndex": 0, "columnIndex": 0 },
                            { "path": "A/2", "rowIndex": 0, "columnIndex": 1 },
                            { "path": "B/1", "rowIndex": 1, "columnIndex": 0 }
                        ]
                    }
                }
            }
        });

        let info = ome_info_v3(&value).expect("an `ome.plate` object makes this a plate");

        assert_eq!(info.tag(), "OME-Zarr 0.5 plate");
        assert_eq!(info.kind.name(), "plate");
        // The counts are the lengths of the declared lists. This fixture
        // declares three wells on a two-by-three plate, and says three -- the
        // occupancy of a plate is not this tool's business.
        assert_eq!(
            group_rows(&NodeKind::Group(GroupMeta {
                ome: Some(info),
                spatialdata: None,
            })),
            vec!["rows: 2", "columns: 3", "wells: 3"]
        );
    }

    #[test]
    fn a_plate_that_declares_no_lists_reports_no_counts() {
        // The marker alone is enough to make it a plate. Counts we cannot take
        // a length from are left out rather than guessed at.
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "ome": { "version": "0.4", "plate": { "rows": "two" } } }
        });

        let info = ome_info_v3(&value).expect("the marker alone makes this a plate");

        assert_eq!(info.tag(), "OME-Zarr 0.4 plate");
        assert!(
            group_rows(&NodeKind::Group(GroupMeta {
                ome: Some(info),
                spatialdata: None,
            }))
            .is_empty()
        );
    }

    #[test]
    fn a_well_is_recognised_and_adds_no_rows() {
        let value = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "ome": {
                    "version": "0.5",
                    "well": { "images": [{ "path": "0" }, { "path": "1" }] }
                }
            }
        });

        let info = ome_info_v3(&value).expect("an `ome.well` object makes this a well");

        assert_eq!(info.tag(), "OME-Zarr 0.5 well");
        assert_eq!(info.kind.name(), "well");
        // A well is tagged and nothing more: the images it lists are the child
        // groups the tree is already printing below it.
        assert!(
            group_rows(&NodeKind::Group(GroupMeta {
                ome: Some(info),
                spatialdata: None,
            }))
            .is_empty()
        );
    }

    #[test]
    fn a_v2_plate_takes_its_version_from_the_plate_object() {
        let dir = fixture("plate-v2");

        // V2 has no `ome` namespace to record a version in, so the version sits
        // inside the `plate` object -- the same split `multiscales` has, in a
        // different place.
        fs::write(dir.join(".zgroup"), r#"{"zarr_format": 2}"#).unwrap();
        fs::write(
            dir.join(".zattrs"),
            r#"{"plate": {"version": "0.4", "rows": [{"name": "A"}], "columns": [{"name": "1"}]}}"#,
        )
        .unwrap();

        let info = group_meta(&dir).ome;

        fs::remove_dir_all(&dir).unwrap();

        let info = info.expect("a V2 `plate` object makes this a plate");
        assert_eq!(info.tag(), "OME-Zarr 0.4 plate");
        // No `wells` list was declared, so no wells row -- the other two stand.
        assert_eq!(
            group_rows(&NodeKind::Group(GroupMeta {
                ome: Some(info),
                spatialdata: None,
            })),
            vec!["rows: 1", "columns: 1"]
        );
    }

    #[test]
    fn plate_and_well_names_alone_mean_nothing() {
        // Everything here *looks* like a plate: a group called `A`, children
        // called `1` and `2`, the word "plate" in a comment field. None of it
        // is a marker, so none of it counts.
        let named_like_a_plate = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "ome": { "version": "0.5", "name": "plate", "well": "A1" } }
        });

        // `ome.well` here is a string, not an object -- a value we cannot read
        // a well out of is not a well.
        assert!(ome_info_v3(&named_like_a_plate).is_none());

        // And the same for a `plate` key that is not an object.
        let plate_is_not_an_object = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "ome": { "version": "0.5", "plate": "96-well" } }
        });

        assert!(ome_info_v3(&plate_is_not_an_object).is_none());
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
        let dir = fixture("sd-labels-v2");

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

        let info = group_meta(&dir).spatialdata;

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
        let dir = fixture("sd-v2");

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

        let info = group_meta(&dir).spatialdata;

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
        let dir = fixture("sd-shapes-v1");

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

        let info = group_meta(&dir).spatialdata;

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
