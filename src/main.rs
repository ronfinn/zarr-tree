use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::process;

use object_store::aws::AmazonS3Builder;
use object_store::http::HttpBuilder;
use object_store::path::Path as ObjectPath;
use object_store::{ClientConfigKey, GetOptions, GetRange, ObjectStore, ObjectStoreExt};
use parquet::basic::LogicalType;
use parquet::file::metadata::{FooterTail, ParquetMetaData, ParquetMetaDataReader};
use parquet::schema::types::Type as SchemaType;
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
/// The first two fields are independent facts, read by two conventions that
/// know nothing about each other. A group may carry either, both, or -- as
/// almost every group does -- neither. A SpatialData raster element carries
/// both, and each field is still read on its own terms.
///
/// The third is not independent of them, and is the one field here that does
/// not come out of a metadata file at all -- see `parquet`.
struct GroupMeta {
    /// Set when the group carries OME-Zarr image metadata. See `ome_info`.
    ome: Option<OmeInfo>,
    /// What this group is in SpatialData's own vocabulary, when it says
    /// anything at all. `None` for every ordinary Zarr group, and for the
    /// `images`/`labels`/`points`/`shapes`/`tables` containers inside a store,
    /// which carry no attributes. See `spatialdata_info`.
    spatialdata: Option<SpatialData>,
    /// What the Parquet payload of a SpatialData points or shapes element
    /// says about itself.
    ///
    /// Read only when `spatialdata` has already said this group is one of
    /// those two, which is what keeps an arbitrary `.parquet` file elsewhere
    /// in a store from being interpreted: a payload is looked for because the
    /// element's own metadata named it, never because a filename looked
    /// promising. `Absent` everywhere else, and `Unavailable` where the
    /// payload should have been readable and was not -- see `parquet_summary`.
    parquet: Payload,
    /// What the AnnData table inside a SpatialData table element declares
    /// about itself.
    ///
    /// Read under the same licence `parquet` is read under, and for the same
    /// reason: only a group whose own metadata already said it is a
    /// SpatialData table is looked into. A group that merely happens to be
    /// called `table`, or to hold children called `X`, `obs` and `var`, is
    /// not -- see `anndata_summary`.
    ///
    /// Boxed, alone among these fields, because of where a `NodeKind` lives:
    /// one is built for every node of the walk and passed around by value, and
    /// this is by far the largest thing a group can carry while being the
    /// rarest -- a store has one table and thousands of ordinary groups. The
    /// box keeps all of them small.
    anndata: Option<Box<AnnData>>,
}

/// What the AnnData object inside a SpatialData table declares about itself.
///
/// Every field here comes out of a Zarr metadata file. AnnData records the
/// shape of the table in metadata -- the length of each dataframe index, the
/// declared column order, the shape and representation of `X` -- so none of it
/// has to be counted, and none of it is. No expression value, no annotation
/// value and no category is read; no chunk under `X`, `obs` or `var` is
/// opened.
///
/// Nothing is checked against anything. A table whose `X` declares a shape its
/// `obs` index disagrees with is reported as it stands, the same way a
/// malformed `shape` is printed as stored: this is inspection, not validation.
struct AnnData {
    /// The version stamped on the group by whichever AnnData wrote it, as
    /// stored. `None` when the group records none.
    encoding_version: Option<String>,
    /// The number of observations: the length of the `obs` index array, or
    /// the first dimension `X` declares when the index could not be read.
    observations: Option<u64>,
    /// The number of variables: the length of the `var` index array, or the
    /// second dimension `X` declares.
    variables: Option<u64>,
    /// How the expression matrix is stored and how big it says it is.
    x: Option<XMatrix>,
    /// The columns `obs` declares in `column-order`, in that order. The index
    /// is not one of them -- AnnData keeps it out of that list -- and neither
    /// is anything counted from the children on disk.
    obs_columns: Option<Vec<String>>,
    /// The columns `var` declares, read the same way.
    var_columns: Option<Vec<String>>,
}

/// How an AnnData `X` is stored, and the shape it declares.
///
/// A dense `X` is one Zarr array, so its own shape and dtype are the answer. A
/// sparse one is a group of three arrays -- `data`, `indices`, `indptr` --
/// none of which is opened: what it is and how big it is are both written in
/// the group's attributes, which is the only thing read.
///
/// No non-zero count appears here. That is not in the metadata, and finding it
/// would mean reading `indptr`.
struct XMatrix {
    /// `dense`, `csr` or `csc`.
    kind: &'static str,
    /// The shape as declared, kept as the values the file held for the same
    /// reason `ArrayMeta` keeps them: the tree wants `[167780, 313]` and
    /// `--json` wants a real JSON array.
    shape: Option<Vec<Value>>,
    /// The element type, which only a dense `X` has here. A sparse `X` keeps
    /// its dtype on the `data` array inside it, and that array is not read.
    dtype: Option<String>,
}

/// What a SpatialData table says about the elements it annotates.
///
/// SpatialData writes these three beside AnnData's own keys in the table
/// group's attributes: which region elements the table annotates, and which
/// two `obs` columns carry the region and instance identifiers. The columns
/// are *named* here; no column value is ever read to reconstruct any of this.
///
/// All three are `None` when the table declares none, which is what a table
/// annotating nothing looks like -- SpatialData writes the keys with a null
/// value rather than leaving them out.
struct TableAnnotation {
    /// The elements annotated. A single element is stored as a bare string
    /// and several as a list; both arrive here as a list -- see `regions`.
    regions: Option<Vec<String>>,
    /// The `obs` column naming the region each observation belongs to.
    region_key: Option<String>,
    /// The `obs` column naming the instance within that region.
    instance_key: Option<String>,
}

/// What a SpatialData element's Parquet payload says about itself, read from
/// the file footer and nowhere else.
///
/// SpatialData keeps the coordinates of a points element and the geometries of
/// a shapes element outside the Zarr hierarchy, in Parquet files beside the
/// element's own metadata. Neither is Zarr, and neither is described by any
/// Zarr metadata: the element declares no path to its payload, so the layout
/// is a convention of SpatialData's writer -- see `payload_files`.
///
/// Every field here comes out of the footer at the end of each file, which is
/// a few kilobytes however large the file is. Nothing below the footer is
/// read: no row group is opened, no page is decoded and no value is looked at.
/// A transcripts payload of well over a gigabyte costs the same handful of
/// kilobytes as a landmark file of three.
struct ParquetSummary {
    /// Rows across every file of the payload, summed. Parquet counts rows in
    /// the footer, so this is read rather than counted.
    rows: i64,
    /// How many files the payload is written across. One for a shapes
    /// element; one per partition for a points element.
    files: usize,
    /// The top-level columns of the first file, in the order the schema
    /// declares them.
    ///
    /// The first file rather than all of them, because the parts of one
    /// payload are one table written in pieces and share a schema. Nested
    /// columns are not expanded: a group column is one column here, as it is
    /// to a reader of the table.
    columns: Vec<ParquetColumn>,
}

/// What became of a SpatialData element's Parquet payload.
///
/// Three answers rather than an `Option<ParquetSummary>`, because "there is
/// nothing there" and "there is something there and we could not read it" are
/// different facts and a reader deserves to be told which one they have. The
/// tree printed the same blank for both until it was pointed out that a points
/// element on a server with no listing looks exactly like one with no payload.
enum Payload {
    /// Nothing to report. Not a points or shapes element at all, or one whose
    /// payload is genuinely not beside it.
    Absent,
    /// A payload that ought to be there and could not be inspected: the parts
    /// could not be listed, or a footer could not be read. Printed as
    /// `parquet files: ?` -- one unavailable marker, since the rows, the width
    /// and the schema are not separately unknown, they are all unknown for the
    /// one reason.
    Unavailable,
    /// Read from the footers, exactly as before.
    Summary(ParquetSummary),
}

/// One column of a Parquet schema: its name, and the type it was written as.
struct ParquetColumn {
    name: String,
    /// Parquet's own name for the type, never a translation into a NumPy or
    /// Arrow spelling -- see `column_type`.
    kind: String,
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
    /// another element, together with what it says it annotates. See
    /// `TableAnnotation`.
    Table(TableAnnotation),
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
            SpatialData::Table(_) => String::from("SpatialData table"),
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
            SpatialData::Table(_) => "table",
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
    /// The names a Zarr V3 array gives its own dimensions, in order.
    ///
    /// Two layers of `Option`, because the format has two distinct absences.
    /// The outer one is the field: V3 makes `dimension_names` optional and V2
    /// has no such key at all, so `None` means "no row to print". The inner
    /// one is a single name: V3 lets an entry be `null`, which says the
    /// dimension exists and is deliberately unnamed. That is not the same as
    /// the array naming nothing, and flattening the two would invent a name
    /// or lose a dimension's position -- so a `null` stays a `None` here,
    /// prints as `?`, and stays `null` in `--json`.
    ///
    /// These are the array's own dimension names, and nothing else. OME-Zarr
    /// `axes` are a separate, higher layer of metadata read from a group's
    /// attributes; neither is derived from the other and an array may carry
    /// both -- see `axis_names`.
    dimension_names: Option<Vec<Option<String>>>,
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
    /// A high-content-screening plate. Each field is the list the metadata
    /// declares, `None` on its own when that list is missing or is not a list
    /// -- nothing here is counted from the directories on disk, so a plate
    /// that declares 96 wells says 96 whether or not 96 were written.
    ///
    /// The first two are lengths because a length is all anything wants of
    /// them. `wells` keeps the paths themselves, and the count printed beside
    /// it is that list's length -- the rule `datasets` follows, and for the
    /// same reason: `--validate` looks for the well each path names, and a
    /// count it would have to re-read the metadata to expand is no use to it.
    Plate {
        rows: Option<usize>,
        columns: Option<usize>,
        wells: Option<Vec<String>>,
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

A store carrying consolidated metadata -- Zarr V2's .zmetadata, or a Zarr V3
root zarr.json holding an inline consolidated_metadata block -- is walked from
that one document, and the rest of the store is never read.

Walking any other HTTP(S) store needs a server that answers WebDAV PROPFIND,
which is how children are found. Metadata is read with ordinary GETs, so a
static server that cannot list can still be inspected with --depth 0, or in
full when the store is consolidated.

OPTIONS:
        --depth <N>  Descend at most N levels below the root.
                     0 shows the root on its own. Omitted, the whole store is
                     walked. Arrays are leaves at any depth.
        --json       Print the same tree as JSON, one object per node.
                     Combines with --depth and with --validate.
        --validate   Check the structure the metadata declares, instead of
                     printing the tree. Reads metadata only, exactly as the
                     tree does. Cannot be combined with --depth: a partial
                     walk would report a node it never looked for as missing.
    -h, --help       Print help
    -V, --version    Print version

EXIT STATUS:
    0  the store was walked; with --validate, nothing worse than a warning
    1  the store could not be read, or the command line made no sense
    2  --validate completed and reported at least one ERROR";

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
    /// Report on the structure the metadata declares instead of printing it.
    /// Reads the same files the tree reads and nothing else -- see `validate`.
    validate: bool,
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
    //
    // A successful run now carries a status of its own, which is 0 for
    // everything this program did before `--validate` existed. Only a
    // validation that found something wrong returns anything else -- see
    // `exit_status`. It is a value rather than a `process::exit` at the point
    // it is decided, so that the output is written and flushed first.
    match run() {
        Ok(0) => {}
        Ok(status) => process::exit(status),
        Err(error) => {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return;
            }
            eprintln!("error: {error}");
            process::exit(1);
        }
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
fn run() -> io::Result<i32> {
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
            return done(&mut out, 0);
        }
        Request::Version => {
            // env! reads the variable when the crate is compiled, so this is a
            // plain string literal in the binary. Cargo fills it in from the
            // version field in Cargo.toml, which is why the two cannot drift
            // apart.
            writeln!(out, "zarr-tree {}", env!("CARGO_PKG_VERSION"))?;
            return done(&mut out, 0);
        }
        Request::Walk(options) => options,
    };

    // Which kind of store this is is settled once, here, by the scheme on the
    // path. Everything below reaches it through `Store` and never asks again.
    let store = open_store(&options.path)?;

    // A store that keeps a copy of all its metadata in one document is read
    // out of that document instead, which is what lets a server with no
    // listing of any kind still be walked in full. Opportunistic: when there
    // is nothing to find, this hands back the store it was given, unchanged.
    // See `ConsolidatedStore`.
    let store = consolidate(store);

    // The root is named by the path as it was typed, in both outputs. Every
    // node below it is named by its directory, or by its S3 prefix.
    let root_name = options.path.trim_end_matches('/');

    // Classified before the root is checked, because the check wants the
    // answer: a root whose metadata named it a Zarr node is plainly there, and
    // one that named nothing has to be looked for. See `Store::check_root`.
    let root_kind = classify(store.as_ref(), "");
    store.check_root(!matches!(root_kind, NodeKind::Unknown))?;

    // The whole of what `--validate` changes, and it changes nothing above
    // this line: the store is opened, consolidated and classified exactly as
    // it is for a tree. What follows reads the same metadata and says
    // something else about it -- see `validate`.
    if options.validate {
        let findings = validate(store.as_ref(), root_kind)?;

        if options.json {
            let report = json_validation(&findings);
            writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        } else {
            print_validation(&mut out, &findings)?;
        }

        return done(&mut out, exit_status(&findings));
    }

    if options.json {
        let tree = json_tree(store.as_ref(), "", root_name, root_kind, options.depth)?;
        // Indented rather than compact: this is still a command a person runs
        // and reads, and `jq` does not mind either way.
        writeln!(out, "{}", serde_json::to_string_pretty(&tree)?)?;
        return done(&mut out, 0);
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
    done(&mut out, 0)
}

/// Flush the output and hand back the status this run ended with.
///
/// Every exit from `run` goes through here, because every one of them ends the
/// same way: stdout flushes itself when the process ends but swallows any
/// error in doing so, and flushing explicitly is what puts a late `BrokenPipe`
/// in front of the handler in `main` instead of losing it. The status rides
/// along so that the two facts a caller needs -- did the writing work, and
/// what should the process exit with -- come back as one value.
fn done(out: &mut dyn Write, status: i32) -> io::Result<i32> {
    out.flush()?;
    Ok(status)
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
    let mut validate = false;

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
            "--validate" => validate = true,
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

    // Refused rather than quietly ignored. A validation walk that stopped
    // early would report every node below the limit as missing -- an OME
    // dataset path, a well, a region -- so the two options cannot both mean
    // what they say at once, and saying so is better than picking one.
    if validate && depth.is_some() {
        return Err(String::from("--depth cannot be combined with --validate"));
    }

    let path = path.ok_or_else(|| String::from("expected a store"))?;
    Ok(Request::Walk(Options {
        path: String::from(path),
        depth,
        json,
        validate,
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
/// Three of the methods are what a walk asks: what does this metadata file
/// say, what lies immediately below this node, and is the root there at all.
/// The other two are not about Zarr at all -- see `files` and `read_suffix`.
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

    /// The names of the files directly inside `path`, sorted.
    ///
    /// The other half of what `children` deliberately throws away, and the
    /// only caller is the one thing in this program that is looking for
    /// something other than a Zarr node: the Parquet parts of a SpatialData
    /// points element, which are files in a directory the element's metadata
    /// names but does not list -- see `payload_files`.
    ///
    /// Kept separate from `children` rather than folded into it because the
    /// distinction is the whole point. `children` must never name a chunk
    /// object, and it never does, because it never looks at files at all.
    ///
    /// An `Err` where a listing cannot be made, which on an ordinary static
    /// HTTP server is always -- see `RemoteStore::diagnose`. The caller takes
    /// that as "no summary" rather than as a failure of the walk.
    fn files(&self, path: &str) -> io::Result<Vec<String>>;

    /// The last `len` bytes of the object at `path`, or all of it when it is
    /// shorter than that. `None` when it is missing or unreadable.
    ///
    /// The one method here that reads something other than metadata, and the
    /// shape of it is the safeguard. Parquet keeps its metadata in a footer at
    /// the very end of the file, so the end is all anybody needs -- and a
    /// method that can only ask for the end cannot be talked into fetching a
    /// four-gigabyte transcripts payload, whoever calls it and however wrong
    /// they are about the size of what they are reading.
    fn read_suffix(&self, path: &str, len: u64) -> Option<Vec<u8>>;
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

/// Read `store`'s consolidated metadata, and hand back a store that answers
/// from it. When there is none to read, hand back the store itself.
///
/// The one place the swap is made, so that everything above it -- the walk,
/// both renderers, every reader of Zarr, OME-Zarr and SpatialData metadata --
/// goes on asking the same questions of the same trait and never learns
/// which kind of store answered.
fn consolidate(store: Box<dyn Store>) -> Box<dyn Store> {
    match ConsolidatedStore::open(store) {
        Ok(consolidated) => Box::new(consolidated),
        // The store back, unchanged. See `ConsolidatedStore::open`.
        Err(store) => store,
    }
}

/// A store's entire metadata, read once out of a single document.
///
/// Some stores keep a copy of every metadata file in the tree in one object at
/// the root. Zarr V2 puts it in `.zmetadata`; Zarr V3 puts a
/// `consolidated_metadata` block inside the root `zarr.json`. Either way one
/// read yields the whole hierarchy, and this is what serves it back.
///
/// That is what makes an ordinary static HTTP server usable. Such a server
/// answers `GET` but not the WebDAV `PROPFIND` a listing needs, so without
/// consolidation the walk cannot get past the root -- see
/// `RemoteStore::diagnose`. With it, no listing is wanted at all: the children
/// are in the document. The same holds for a local directory and for S3, where
/// it saves a request per node rather than making the walk possible.
///
/// For Zarr metadata this holds no store: once the document has been read the
/// index *is* the store, and `read`, `children` and `check_root` answer from
/// it and from nothing else. Consolidated metadata is a snapshot, taken at one
/// moment and possibly stale since; a tree that mixed it with live reads would
/// show two moments at once and say which was which nowhere. No fallback, in
/// either direction: a metadata file the document does not name is missing.
///
/// The physical store is kept all the same, for the one thing the document
/// cannot answer. A SpatialData element's Parquet payload is not Zarr, is not
/// listed in any consolidated document, and has to be read where it lies --
/// see `files` and `read_suffix` below. That is the whole of what the store
/// behind this is used for, and the split is what keeps the snapshot whole
/// while the payload stays readable.
struct ConsolidatedStore {
    /// Metadata path to document text, in exactly the form `Store::read`
    /// takes: `.zgroup`, `images/0/.zarray`, `a/b/zarr.json`.
    ///
    /// The text is written back out from the parsed document rather than
    /// sliced from the original, because JSON gives no way to point at a
    /// subtree of itself. What every reader above wants is the object, and the
    /// object is unchanged; only its whitespace is.
    documents: BTreeMap<String, String>,
    /// Node path to its immediate children, sorted. Derived from the metadata
    /// paths and from nothing else -- see `child_index`.
    children: BTreeMap<String, Vec<String>>,
    /// The store the document was read off, kept for binary payloads alone.
    ///
    /// Nothing about the Zarr hierarchy is ever asked of it again. See the
    /// type's own note for why that line is drawn where it is.
    physical: Box<dyn Store>,
}

impl ConsolidatedStore {
    /// A store built over `store`'s consolidated metadata, if it has any this
    /// program reads -- and `store` itself back if it has not.
    ///
    /// The two conventions are tried in the order the two Zarr versions are
    /// tried everywhere else here. An `Err` means there was nothing to find,
    /// or nothing that could be made sense of, and the walk then reads the
    /// store directly exactly as it did before any of this existed.
    /// Consolidation is opportunistic: no store that worked without it may
    /// come to depend on it.
    ///
    /// `Result<_, Box<dyn Store>>` rather than an `Option`, because this takes
    /// ownership of the store and the caller still needs it when the answer is
    /// no. Handing it back inside the `Err` is how Rust says "I did not use
    /// this after all"; an `Option` would have dropped it and left the caller
    /// with nothing to fall back to.
    fn open(store: Box<dyn Store>) -> Result<ConsolidatedStore, Box<dyn Store>> {
        let Some(documents) =
            consolidated_v2(store.as_ref()).or_else(|| consolidated_v3(store.as_ref()))
        else {
            return Err(store);
        };

        Ok(ConsolidatedStore {
            children: child_index(&documents),
            documents,
            physical: store,
        })
    }
}

impl Store for ConsolidatedStore {
    fn read(&self, path: &str) -> Option<String> {
        // The index and nothing but the index. A metadata file the document
        // does not mention is missing as far as this store is concerned, which
        // is what keeps the snapshot whole -- see the type's own note.
        self.documents.get(path).cloned()
    }

    fn children(&self, path: &str) -> io::Result<Vec<String>> {
        // Never an error. The listing that could fail is the very thing
        // consolidation replaces, and a node the index does not know has no
        // children rather than an unanswerable question.
        Ok(self.children.get(path).cloned().unwrap_or_default())
    }

    fn check_root(&self, _identified: bool) -> io::Result<()> {
        // The document was read off the store, so the root is there. That is
        // the whole check, and it is the one a server without a listing could
        // not otherwise have passed.
        Ok(())
    }

    fn files(&self, path: &str) -> io::Result<Vec<String>> {
        // Not Zarr, so not in the document, so asked of the store. On the very
        // server consolidation exists to rescue this still cannot be answered,
        // and the caller treats that as "no summary" -- see `payload_files`.
        self.physical.files(path)
    }

    fn read_suffix(&self, path: &str, len: u64) -> Option<Vec<u8>> {
        // Likewise: a Parquet footer is bytes in a file, and no consolidated
        // document has ever carried one.
        self.physical.read_suffix(path, len)
    }
}

/// The metadata map of a Zarr V2 `.zmetadata`, keyed by metadata path.
///
/// The document is flat: one entry per metadata file in the store, keyed by
/// the very path a walk would have read that file from. So there is no
/// translation to do here, only filtering -- see `metadata_node`.
///
/// `zarr_consolidated_format` is a version, and 1 is the only one there has
/// ever been. Anything else is left alone rather than guessed at: falling back
/// to reading the store is slower and always right.
fn consolidated_v2(store: &dyn Store) -> Option<BTreeMap<String, String>> {
    let value: Value = serde_json::from_str(&store.read(".zmetadata")?).ok()?;

    if value.get("zarr_consolidated_format")?.as_u64()? != 1 {
        return None;
    }

    let mut documents = BTreeMap::new();
    for (path, document) in value.get("metadata")?.as_object()? {
        // Only the filenames a node's metadata lives in. Anything else in the
        // map -- a chunk key, something another tool left there -- is not a
        // document this program can read and must not become a node.
        if metadata_node(path).is_some() {
            documents.insert(path.clone(), document.to_string());
        }
    }

    Some(documents)
}

/// The nodes of a Zarr V3 root `zarr.json` that carries an inline
/// `consolidated_metadata` block, keyed by metadata path.
///
/// V3 consolidation is younger than V2's and less settled -- zarr-python warns
/// that it is not part of the V3 specification and may change -- so only the
/// one form current zarr-python actually writes is read here. Anything else
/// falls back to reading the store.
///
/// The root's own document goes in as it was read: it is the one thing in the
/// index that came off the store rather than out of the block, and it is what
/// classifies the root.
fn consolidated_v3(store: &dyn Store) -> Option<BTreeMap<String, String>> {
    let root = store.read("zarr.json")?;
    let value: Value = serde_json::from_str(&root).ok()?;
    let block = value.get("consolidated_metadata")?;

    // Checked before anything is built, so that an unsupported block is a
    // fallback to live reads rather than a half-filled index.
    inline_metadata(block)?;

    let mut documents = BTreeMap::new();
    documents.insert(String::from("zarr.json"), root);
    collect_v3(block, "", &mut documents);
    Some(documents)
}

/// The nodes named by one `consolidated_metadata` block, or `None` when the
/// block is not one this program reads.
///
/// `kind` says how the metadata is carried and `inline` -- in the block itself
/// -- is the only value zarr-python writes. `must_understand` is the Zarr V3
/// escape hatch: `false` means a reader that does not understand this block
/// may ignore it, which is exactly what returning `None` here does. `true`
/// would be a demand, and the honest answer to a demand we cannot meet is to
/// leave the block alone.
fn inline_metadata(block: &Value) -> Option<&serde_json::Map<String, Value>> {
    if block.get("kind")?.as_str()? != "inline" {
        return None;
    }
    if block.get("must_understand")?.as_bool()? {
        return None;
    }
    block.get("metadata")?.as_object()
}

/// Add the nodes of one `consolidated_metadata` block to `documents`, under
/// `prefix`.
///
/// Each entry is a whole `zarr.json` document, and its key is that node's path
/// relative to the group holding the block. Current zarr-python writes the
/// flat form -- `images`, `images/0`, `a/b/arr` all in the root's block, each
/// nested block left empty -- but a group's block is defined to hold its own
/// children, so a non-empty one is followed. Both are the same rule read
/// twice, which is why one function does both.
///
/// A nested block that is not the inline form stops the recursion there and
/// nowhere else: the nodes already collected stand, and the subtree beneath
/// the block simply has no children in the index.
fn collect_v3(block: &Value, prefix: &str, documents: &mut BTreeMap<String, String>) {
    let Some(nodes) = inline_metadata(block) else {
        return;
    };

    for (name, node) in nodes {
        let path = child_path(prefix, name);
        documents.insert(child_path(&path, "zarr.json"), node.to_string());

        if let Some(nested) = node.get("consolidated_metadata") {
            collect_v3(nested, &path, documents);
        }
    }
}

/// The node a metadata path belongs to, or `None` when the path does not name
/// a metadata file this program reads.
///
/// `images/0/.zarray` belongs to `images/0`, and `.zgroup` belongs to the
/// root, which is the empty string. The filenames are never nodes themselves:
/// a store's tree is its groups and arrays, not the files describing them.
fn metadata_node(path: &str) -> Option<&str> {
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    };

    match name {
        ".zgroup" | ".zarray" | ".zattrs" | "zarr.json" => Some(parent),
        _ => None,
    }
}

/// Which children each node has, worked out from the metadata paths alone.
///
/// Every path above a node is registered as a node too. A group whose own
/// metadata is missing from the document is then still there -- classified
/// `[unknown]`, which is the right answer and the same one the walk gives an
/// unreadable directory -- rather than taking the whole subtree beneath it out
/// of the tree.
///
/// The root is not a child of anything, so it is the one node that never
/// appears in a list here; it appears only as a key.
fn child_index(documents: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
    let mut nodes: BTreeSet<&str> = BTreeSet::new();
    for path in documents.keys() {
        let Some(node) = metadata_node(path) else {
            continue;
        };
        for (index, _) in node.match_indices('/') {
            nodes.insert(&node[..index]);
        }
        if !node.is_empty() {
            nodes.insert(node);
        }
    }

    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in nodes {
        let (parent, name) = match node.rsplit_once('/') {
            Some((parent, name)) => (parent, name),
            None => ("", node),
        };
        children
            .entry(String::from(parent))
            .or_default()
            .push(String::from(name));
    }

    // Sorted for the reason every other `children` is: the tree has to know
    // which child is last before it can draw a connector. The set they came
    // out of was ordered by full path, which for one parent's children is the
    // same order -- but that is a coincidence of the encoding, not a promise
    // this makes, so it is said here rather than relied on.
    for names in children.values_mut() {
        names.sort();
    }

    children
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

    fn files(&self, path: &str) -> io::Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(self.resolve(path))? {
            let entry = entry?;
            // The mirror image of `children`, which keeps the directories.
            if !entry.file_type()?.is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    fn read_suffix(&self, path: &str, len: u64) -> Option<Vec<u8>> {
        let mut file = fs::File::open(self.resolve(path)).ok()?;
        let size = file.metadata().ok()?.len();

        // Clamped rather than seeking backwards from the end, because seeking
        // past the start of a file is an error and a short file is the
        // ordinary case: a landmark payload is three kilobytes.
        let start = size.saturating_sub(len);
        file.seek(io::SeekFrom::Start(start)).ok()?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(bytes)
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

    fn files(&self, path: &str) -> io::Result<Vec<String>> {
        let listing = self.list(&self.key(path))?;

        // The objects this time, where `children` takes the common prefixes.
        // Same request, other half of the answer.
        let mut names: Vec<String> = listing
            .objects
            .iter()
            .filter_map(|object| object.location.filename())
            .map(String::from)
            .collect();
        names.sort();
        Ok(names)
    }

    fn read_suffix(&self, path: &str, len: u64) -> Option<Vec<u8>> {
        let key = self.key(path);

        // The size first, because the range asked for has to be one the object
        // really has. `object_store` checks the answer against the request and
        // refuses a reply that does not match, so a range running past the end
        // of the object is an error rather than a short read -- and a suffix
        // range, which would have avoided this request, is a thing not every
        // static server answers.
        let size = self
            .runtime
            .block_on(self.store.head(&key))
            .ok()
            .map(|meta| meta.size)?;

        let start = size.saturating_sub(len);
        let options = GetOptions {
            range: Some(GetRange::Bounded(start..size)),
            ..GetOptions::default()
        };

        let bytes = self.runtime.block_on(async {
            self.store
                .get_opts(&key, options)
                .await
                .ok()?
                .bytes()
                .await
                .ok()
        })?;

        // Noted for one error message, and nothing else. See `reachable`.
        self.reachable.set(true);

        Some(bytes.to_vec())
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
        _ => print_tree(out, store, "", "", kind, depth),
    }
}

/// Print the children of the node at `path`, one line each, indented by
/// `prefix`.
///
/// `store` is where the metadata comes from, and the only thing here that
/// knows whether these nodes are directories or S3 prefixes. `path` names the
/// node inside it -- see `Store`.
///
/// `kind` is what the node was classified as. Its metadata rows are drawn here
/// rather than by the caller because this is the one place that already knows
/// whether any children follow them, which is what decides the last connector.
/// It also decides which children there are -- see `child_dirs`.
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
    kind: &NodeKind,
    depth: Option<usize>,
) -> io::Result<()> {
    let rows = group_rows(kind);
    let children = child_dirs(store, path, kind, depth)?;

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
                &kind,
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
///
/// `kind` is here for one case. A SpatialData points element keeps its payload
/// in a directory beside its metadata, and that directory is not a Zarr node:
/// listed as a child it would show as `[unknown]`, directly above the rows
/// that already say what is in it. So it is dropped -- but only from a node
/// whose own metadata said it is a points element, never from a directory that
/// merely happens to be called that.
fn child_dirs(
    store: &dyn Store,
    path: &str,
    kind: &NodeKind,
    depth: Option<usize>,
) -> io::Result<Vec<String>> {
    if depth == Some(0) {
        return Ok(Vec::new());
    }

    let mut names = store.children(path)?;

    if let NodeKind::Group(GroupMeta {
        spatialdata: Some(SpatialData::Points),
        ..
    }) = kind
    {
        names.retain(|name| name != "points.parquet");
    }

    Ok(names)
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
        let spatialdata = attrs.as_ref().and_then(spatialdata_info_v2);
        return NodeKind::Group(GroupMeta {
            ome: attrs.as_ref().and_then(ome_info_v2),
            // Read after the attributes and only because of them: what the
            // metadata said this element is decides whether there is a payload
            // to look for at all.
            parquet: parquet_summary(store, path, spatialdata.as_ref()),
            // And the same again for the AnnData table inside a table
            // element, which the same answer licenses.
            anndata: anndata_summary(store, path, attrs.as_ref(), spatialdata.as_ref()),
            spatialdata,
        });
    }

    if let Some(zarray) = store.read(&child_path(path, ".zarray")) {
        return NodeKind::Array(array_meta_v2(&zarray));
    }

    // Zarr V3 uses one filename for both kinds and moves the distinction inside
    // the file, so here we do have to look inside it. Checked second, so a
    // store that carries both V2 and V3 metadata is reported as V2.
    if let Some(kind) = classify_v3(store, path) {
        return kind;
    }

    NodeKind::Unknown
}

/// Read `node_type` out of the Zarr V3 `zarr.json` of the node at `path`, or
/// `None` if the file is missing, unreadable, not valid JSON, or has no
/// recognisable `node_type`.
///
/// Takes the node rather than the file, unlike the V2 reads above, because a
/// SpatialData element's Parquet payload sits beside the node's metadata file
/// rather than inside it, and finding it means knowing where the node is.
fn classify_v3(store: &dyn Store, path: &str) -> Option<NodeKind> {
    let value = read_json(store, &child_path(path, "zarr.json"))?;

    match value.get("node_type")?.as_str()? {
        "group" => {
            let spatialdata = spatialdata_info_v3(&value);
            Some(NodeKind::Group(GroupMeta {
                ome: ome_info_v3(&value),
                parquet: parquet_summary(store, path, spatialdata.as_ref()),
                anndata: anndata_summary(
                    store,
                    path,
                    value.get("attributes"),
                    spatialdata.as_ref(),
                ),
                spatialdata,
            }))
        }
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
                // The same `{"path": ...}` list a multiscale's `datasets` is,
                // so it is read by the same function -- see `dataset_paths`.
                wells: dataset_paths(plate.get("wells")),
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
///
/// A table carries what it annotates in the same attributes object, so it is
/// read here rather than by a second pass: the keys sit beside the one that
/// just proved this is a table, and reading them costs nothing.
fn encoded_kind(attrs: &Value, key: &str) -> Option<SpatialData> {
    match attrs.get(key)?.as_str()? {
        "ngff:points" => Some(SpatialData::Points),
        "ngff:shapes" => Some(SpatialData::Shapes),
        "ngff:regions_table" => Some(SpatialData::Table(table_annotation(attrs))),
        _ => None,
    }
}

/// What a table's attributes say it annotates.
///
/// The three keys sit at the top level of the table group's attributes, beside
/// AnnData's own, which is where every release of SpatialData that writes them
/// has put them -- in Zarr V2 and V3 alike. A table that annotates nothing
/// still has all three, written as null, and each falls out here as `None` on
/// its own.
///
/// Nothing is read from `obs` to fill any of this in. `region_key` and
/// `instance_key` are the *names* of two columns, and names are all that is
/// reported: the columns themselves are chunk data.
fn table_annotation(attrs: &Value) -> TableAnnotation {
    TableAnnotation {
        regions: regions(attrs.get("region")),
        region_key: text(attrs.get("region_key")),
        instance_key: text(attrs.get("instance_key")),
    }
}

/// The elements a table annotates, as a list however the file wrote them.
///
/// SpatialData stores a single element as a bare string and several as a list,
/// and the difference says nothing: both mean "these are the regions". They
/// are levelled here so that neither renderer has to know about the two
/// spellings, and so that the tree and `--json` cannot disagree about which
/// one they are looking at.
///
/// An entry that is not a string becomes `?` rather than being dropped, the
/// same way `dataset_paths` treats a nameless dataset: dropping it would
/// report a table over three regions as one over two.
fn regions(value: Option<&Value>) -> Option<Vec<String>> {
    match value? {
        Value::String(one) => Some(vec![one.clone()]),
        Value::Array(items) if !items.is_empty() => Some(
            items
                .iter()
                .map(|item| match item.as_str() {
                    Some(name) => String::from(name),
                    None => String::from("?"),
                })
                .collect(),
        ),
        _ => None,
    }
}

/// A string-valued attribute, or `None` when it is missing, null, or not a
/// string.
fn text(value: Option<&Value>) -> Option<String> {
    Some(String::from(value?.as_str()?))
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

/// How much of the end of a Parquet file to ask for first.
///
/// The footer is a length and a magic word in the last eight bytes, and the
/// metadata itself sits immediately above them. Reading one generous piece of
/// the end finds both in a single request nearly always; the fallback for a
/// schema too large for it is one more read of exactly the right size, and no
/// more than that -- see `parquet_metadata`.
///
/// Sixty-four kilobytes covers every payload SpatialData writes by a wide
/// margin: the eight-part, gigabyte-and-a-half Xenium transcripts payload has
/// footers of about three.
const PARQUET_TAIL: u64 = 64 * 1024;

/// The Parquet payload of a SpatialData points or shapes element, as far as
/// it can be made out.
///
/// `element` is what the group's own metadata said it is, and it is the whole
/// of the licence to go looking. Every other kind of node -- an ordinary Zarr
/// group, a SpatialData image, a store root -- returns `Absent` here at once,
/// which is what keeps this from wandering into files it was never told
/// about. A `.parquet` file sitting somewhere else in a store is not a payload
/// and is not read: this program never infers meaning from a name.
///
/// The two failing answers are told apart by what the store was able to say,
/// and by nothing else -- see `Payload` and `unreadable`.
fn parquet_summary(store: &dyn Store, path: &str, element: Option<&SpatialData>) -> Payload {
    let Some(element) = element else {
        return Payload::Absent;
    };

    let files = match payload_files(store, path, element) {
        Ok(files) if files.is_empty() => return Payload::Absent,
        Ok(files) => files,
        // The listing failed, so there may well be a payload there and we
        // cannot see it. Only a points element ever gets this far, because
        // only a points payload needs listing at all.
        Err(_) => return Payload::Unavailable,
    };

    let mut rows = 0i64;
    let mut columns = Vec::new();

    for (index, file) in files.iter().enumerate() {
        let Some(metadata) = parquet_metadata(store, file) else {
            return unreadable(element);
        };
        let file_metadata = metadata.file_metadata();

        rows += file_metadata.num_rows();
        // The schema comes off the first part. The parts of one payload are
        // one table written in pieces, so the later ones have nothing to add.
        if index == 0 {
            columns = schema_columns(file_metadata.schema());
        }
    }

    Payload::Summary(ParquetSummary {
        rows,
        files: files.len(),
        columns,
    })
}

/// What a payload file we could not read the footer of amounts to, which is
/// not the same thing for the two element kinds.
///
/// A points part was named by a listing, so the payload is demonstrably there
/// and what failed is the inspection of it. A shapes file was named by nobody
/// -- the convention supplied the filename -- and `read_suffix` answers
/// `None` whether the file is missing, is not Parquet, or has an encrypted
/// footer. With no way to tell those apart, absence stays the honest reading.
fn unreadable(element: &SpatialData) -> Payload {
    match element {
        SpatialData::Points => Payload::Unavailable,
        _ => Payload::Absent,
    }
}

/// Where a SpatialData element keeps its Parquet payload, in the order the
/// files should be read.
///
/// The two element kinds do not agree about this, and the difference is not
/// cosmetic. A shapes element is a GeoDataFrame written by geopandas in one
/// go, so `shapes.parquet` is a file. A points element is a dask DataFrame
/// written a partition at a time, so `points.parquet` is a *directory* of
/// `part.0.parquet`, `part.1.parquet` and so on -- one file where the frame
/// had one partition, eight where the Xenium transcripts have eight.
///
/// Neither path is declared anywhere in the element's metadata; both are
/// conventions of SpatialData's writer, and are hard-coded here for that
/// reason. That is not an inference from a directory name: the element said
/// what it is, and this is where that kind of element keeps its payload.
///
/// A points payload therefore needs a file listing, and a shapes payload does
/// not. On a plain static HTTP server, which can answer a `GET` but no kind of
/// listing at all, that is the difference between a shapes element that
/// summarises and a points element that cannot be inspected.
///
/// An empty list is a payload that is not there. `Err` is the other thing
/// entirely -- a listing we could not get -- and only a points element can
/// produce one.
fn payload_files(store: &dyn Store, path: &str, element: &SpatialData) -> io::Result<Vec<String>> {
    match element {
        SpatialData::Shapes => Ok(vec![child_path(path, "shapes.parquet")]),
        SpatialData::Points => {
            let directory = child_path(path, "points.parquet");
            let mut names = match store.files(&directory) {
                Ok(names) => names,
                // A directory that is not there is a payload that is not
                // there, and that much a local store says precisely. Every
                // other failure is a listing we did not get, which says
                // nothing at all about whether a payload exists.
                Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error),
            };

            // dask writes only the parts unless it is asked for a metadata
            // file as well, but it can be asked, and `_metadata` is not a part
            // and has no rows to count.
            names.retain(|name| name.ends_with(".parquet"));

            // Already sorted by `files`, which is lexicographic and so orders
            // ten parts `part.0, part.1, part.10, part.2`. Nothing here cares:
            // the rows are summed and the schema is one schema.
            Ok(names
                .iter()
                .map(|name| child_path(&directory, name))
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

/// Read one Parquet file's footer metadata, and nothing else.
///
/// Parquet puts its metadata at the *end* of the file: a thrift-encoded block,
/// then that block's length, then `PAR1`. So the end is read first -- one
/// piece large enough to hold both the eight-byte tail and, nearly always, the
/// block above it. A schema too large for that costs one more read, of exactly
/// the length the tail just named.
///
/// Two reads at worst, of a few kilobytes each, whether the file is three
/// kilobytes or two gigabytes. No row group, no page, no column chunk and no
/// value is touched: `decode_metadata` is handed the footer bytes and stops
/// there.
fn parquet_metadata(store: &dyn Store, path: &str) -> Option<ParquetMetaData> {
    let tail = store.read_suffix(path, PARQUET_TAIL)?;
    let footer: &[u8] = tail.get(tail.len().checked_sub(8)?..)?;

    // Rejects a file that does not end in `PAR1`, and says so for one that
    // ends in `PARE` -- an encrypted footer, which `decode_metadata` would
    // refuse anyway and which this program has no key for.
    let length = FooterTail::try_from(footer).ok()?.metadata_length();

    // The block sits immediately above the eight bytes just read.
    let wanted = length.checked_add(8)?;
    let bytes = if tail.len() >= wanted {
        tail[tail.len() - wanted..tail.len() - 8].to_vec()
    } else {
        let tail = store.read_suffix(path, wanted as u64)?;
        tail.get(tail.len().checked_sub(wanted)?..tail.len() - 8)?
            .to_vec()
    };

    ParquetMetaDataReader::decode_metadata(&bytes).ok()
}

/// What the AnnData table at `path` declares about itself, or `None` when
/// there is nothing to report.
///
/// `element` is what the group's own metadata said it is, and -- exactly as in
/// `parquet_summary` -- it is the whole of the licence to go looking. Every
/// other kind of node returns here at once. That is what keeps an ordinary
/// Zarr group holding children called `X`, `obs` and `var` from being read as
/// an AnnData table: the recognition rule is the SpatialData marker, and this
/// only interprets what that rule has already admitted.
///
/// `attrs` is the table group's own attributes, already in hand from
/// `classify`, which is where AnnData's `encoding-version` sits.
///
/// Five metadata files at most are opened below this node -- `obs`, `var`, the
/// index array each of them names, and `X` -- and nothing else. No listing is
/// made: every path here is named by a metadata file, so the summary costs the
/// same handful of reads on a static HTTP server as it does on a local disk,
/// and comes wholly out of the snapshot when the store is consolidated.
///
/// Each field falls out on its own. A table whose `var` cannot be read still
/// reports the observations its `obs` declared.
fn anndata_summary(
    store: &dyn Store,
    path: &str,
    attrs: Option<&Value>,
    element: Option<&SpatialData>,
) -> Option<Box<AnnData>> {
    if !matches!(element?, SpatialData::Table(_)) {
        return None;
    }

    // Recorded rather than checked. A table whose group says `0.2.0`, or says
    // nothing at all, is reported as it stands.
    let encoding_version = text(attrs.and_then(|attrs| attrs.get("encoding-version")));

    let obs = dataframe(store, &child_path(path, "obs"));
    let var = dataframe(store, &child_path(path, "var"));
    let x = x_matrix(store, &child_path(path, "X"));

    // The index array's length is the axis length, and it is metadata. `X`'s
    // declared shape is the fallback for a dataframe we could not read, which
    // costs nothing: `X` has already been read. The two are not compared --
    // whichever answered first is the answer.
    let shape = x.as_ref().and_then(|x| x.shape.as_deref());
    let observations = obs.as_ref().and_then(|obs| obs.length).or(dim(shape, 0));
    let variables = var.as_ref().and_then(|var| var.length).or(dim(shape, 1));

    Some(Box::new(AnnData {
        encoding_version,
        observations,
        variables,
        obs_columns: obs.and_then(|obs| obs.columns),
        var_columns: var.and_then(|var| var.columns),
        x,
    }))
}

/// One dimension of a declared shape, when it is a number we can use.
fn dim(shape: Option<&[Value]>, axis: usize) -> Option<u64> {
    shape?.get(axis)?.as_u64()
}

/// What an AnnData dataframe group -- `obs` or `var` -- declares about itself.
///
/// Both fields are `None` on their own: a dataframe whose `column-order` is
/// unreadable still yields the length its index declared.
struct DataFrame {
    /// The length of the index array the group names, which is the length of
    /// the axis. `None` when no index was named or its metadata could not be
    /// read.
    length: Option<u64>,
    /// The declared `column-order`, in that order.
    columns: Option<Vec<String>>,
}

/// Read an AnnData dataframe group's declared shape, without reading a column.
///
/// AnnData writes two things here that between them describe the dataframe
/// entirely: `column-order`, which is the columns, and `_index`, which names
/// the array holding the index. The index array's own Zarr metadata carries
/// its length, and that length is the axis length -- so `n_obs` is read from a
/// `shape` field rather than counted from anything.
///
/// The columns are taken from `column-order` alone and never from the children
/// on disk. The two are usually the same list, but only one of them is what
/// the dataframe *declares*, and a listing would also sweep up the index array
/// and the `categories`/`codes` groups of a categorical column.
///
/// Two reads: the group, and the one array it named. Neither array is opened.
fn dataframe(store: &dyn Store, path: &str) -> Option<DataFrame> {
    let node = anndata_node(store, path)?;

    // An entry that is not a string becomes `?` rather than being dropped, so
    // the count stays the count the file declared.
    let columns = node
        .attrs
        .get("column-order")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| match item.as_str() {
                    Some(name) => String::from(name),
                    None => String::from("?"),
                })
                .collect()
        });

    let length = text(node.attrs.get("_index"))
        .and_then(|name| anndata_node(store, &child_path(path, &name)))
        .and_then(|index| {
            let shape = index.array?.shape?;
            shape.first()?.as_u64()
        });

    Some(DataFrame { length, columns })
}

/// How the expression matrix at `path` is stored, or `None` when it is neither
/// a dense array nor a sparse representation this understands.
///
/// One read. A dense `X` is a Zarr array and answers out of its own metadata;
/// a sparse one is a group whose attributes carry both the representation and
/// the shape. Either way the arrays holding the values -- `X` itself, or
/// `data`, `indices` and `indptr` beneath it -- are never opened.
fn x_matrix(store: &dyn Store, path: &str) -> Option<XMatrix> {
    let node = anndata_node(store, path)?;

    // A dense X is an array, and being an array is what makes it dense.
    // AnnData also stamps it `encoding-type: "array"`, which says the same
    // thing one file further away in Zarr V2; the node kind is read instead
    // because it is already in hand.
    if let Some(array) = node.array {
        return Some(XMatrix {
            kind: "dense",
            shape: array.shape,
            dtype: array.dtype,
        });
    }

    // Matched exactly. A group under `X` declaring anything else is a
    // representation this version does not know, and is reported as no row at
    // all rather than guessed at.
    let kind = match node.attrs.get("encoding-type")?.as_str()? {
        "csr_matrix" => "csr",
        "csc_matrix" => "csc",
        _ => return None,
    };

    Some(XMatrix {
        kind,
        shape: dims(node.attrs.get("shape")),
        dtype: None,
    })
}

/// One node inside an AnnData table, read the cheapest way its Zarr version
/// allows.
///
/// Both things wanted from such a node come from here: its user attributes,
/// where AnnData's own vocabulary lives, and its shape, which only an array
/// has. `classify` cannot serve this -- it throws the attributes away, keeping
/// only what OME-Zarr and SpatialData made of them.
struct AnnDataNode {
    /// The node's user attributes. `Value::Null` when it has none, which reads
    /// every key as missing without a branch of its own.
    attrs: Value,
    /// The array metadata, and so also the proof that this is an array.
    /// `None` for a group.
    array: Option<ArrayMeta>,
}

/// Read one node of an AnnData table: its attributes, and its shape when it
/// has one.
///
/// Zarr V3 first, because that is what every SpatialData store since 0.7
/// writes and because it answers both questions from one file. V2 splits them,
/// and which of its two files is there is what says whether this is an array.
fn anndata_node(store: &dyn Store, path: &str) -> Option<AnnDataNode> {
    if let Some(value) = read_json(store, &child_path(path, "zarr.json")) {
        let array = match value.get("node_type").and_then(|kind| kind.as_str()) {
            Some("array") => Some(array_meta_v3(&value)),
            _ => None,
        };
        let attrs = value.get("attributes").cloned().unwrap_or(Value::Null);
        return Some(AnnDataNode { attrs, array });
    }

    // V2 keeps user attributes in `.zattrs` whichever kind of node this is, so
    // it is read either way; `.zarray` is what adds the shape.
    let attrs = read_json(store, &child_path(path, ".zattrs"));
    if let Some(zarray) = store.read(&child_path(path, ".zarray")) {
        return Some(AnnDataNode {
            attrs: attrs.unwrap_or(Value::Null),
            array: Some(array_meta_v2(&zarray)),
        });
    }

    Some(AnnDataNode {
        attrs: attrs?,
        array: None,
    })
}

/// The top-level columns of a Parquet schema, in declaration order.
///
/// The root of a Parquet schema is a group whose fields are the columns a
/// reader of the table sees. Its *leaves* are something else: a nested column
/// counts once here and several times there, and it is the reader's count that
/// belongs beside a row count.
fn schema_columns(schema: &SchemaType) -> Vec<ParquetColumn> {
    schema
        .get_fields()
        .iter()
        .map(|field| ParquetColumn {
            name: String::from(field.name()),
            kind: column_type(field),
        })
        .collect()
}

/// What a column was written as, in Parquet's own vocabulary.
///
/// Parquet describes a column twice: a physical type, which is how the bytes
/// are laid down, and an optional logical type, which is what they mean. The
/// logical one is the answer where there is one -- a `feature_name` is a
/// `string`, not a `byte_array` -- and the physical one otherwise.
///
/// Reported, not translated. These are the spellings the file itself uses, and
/// no attempt is made to render them as the NumPy or Arrow names some other
/// tool would show, for the same reason a Zarr `dtype` is printed as stored.
fn column_type(field: &SchemaType) -> String {
    let logical = match field.get_basic_info().logical_type_ref() {
        Some(LogicalType::String) => Some(String::from("string")),
        Some(LogicalType::Enum) => Some(String::from("enum")),
        Some(LogicalType::Uuid) => Some(String::from("uuid")),
        Some(LogicalType::Json) => Some(String::from("json")),
        Some(LogicalType::Bson) => Some(String::from("bson")),
        Some(LogicalType::Date) => Some(String::from("date")),
        Some(LogicalType::Time(_)) => Some(String::from("time")),
        Some(LogicalType::Timestamp(_)) => Some(String::from("timestamp")),
        Some(LogicalType::Float16) => Some(String::from("float16")),
        Some(LogicalType::Map) => Some(String::from("map")),
        Some(LogicalType::List) => Some(String::from("list")),
        Some(LogicalType::Integer(int)) => Some(format!(
            "{}int{}",
            if int.is_signed { "" } else { "u" },
            int.bit_width
        )),
        Some(LogicalType::Decimal(decimal)) => {
            Some(format!("decimal({}, {})", decimal.precision, decimal.scale))
        }
        // Every other logical type, including whatever a future version of the
        // format adds, falls through to the physical one. That is always true
        // and never misleading, which is the right way to be wrong here.
        _ => None,
    };

    logical.unwrap_or_else(|| match field.is_primitive() {
        // `BYTE_ARRAY`, `DOUBLE`, `INT64`. Lowercased only because that is how
        // every other value this program prints looks.
        true => field.get_physical_type().to_string().to_lowercase(),
        // A nested column with no logical type of its own. There is no
        // physical type to give: the bytes are in the leaves below.
        false => String::from("group"),
    })
}

/// A count with its thousands separated, so that a row count can be read.
///
/// The one number this program prints that has no bound on it. A store's
/// shapes and its pyramid levels are counted in tens; a transcripts payload is
/// counted in millions, and `4825319` is not a number anybody reads.
fn grouped(count: i64) -> String {
    let digits = count.abs().to_string();
    let mut text = String::new();

    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }

    match count < 0 {
        true => format!("-{text}"),
        false => text,
    }
}

/// The metadata rows to print underneath a node's own line, in display order.
///
/// A group with neither OME-Zarr metadata nor a Parquet payload has nothing to
/// say here and gets an empty list. Returning one list rather than one
/// argument per row keeps `print_tree` from growing a parameter every time a
/// row is added: all it needs to know is how many rows there are and what each
/// one says.
fn group_rows(kind: &NodeKind) -> Vec<String> {
    // `let ... else` matches one pattern and takes an early exit when it does
    // not fit, which reads better here than a `match` whose other arm is just
    // an empty list.
    let NodeKind::Group(meta) = kind else {
        return Vec::new();
    };

    // Order is display order and nothing more. The four readers are
    // independent, and no group has anything to say to more than two of them:
    // a table has AnnData rows and annotation rows, a points element has
    // Parquet rows, and neither has the other's.
    let mut rows = ome_rows(meta.ome.as_ref());
    rows.extend(anndata_rows(meta.anndata.as_deref()));
    rows.extend(table_rows(meta.spatialdata.as_ref()));
    rows.extend(parquet_rows(&meta.parquet));
    rows
}

/// The rows an AnnData table adds beneath its own line.
///
/// The shape of the table, in the order somebody reading it would ask: how
/// many observations, how many variables, how the matrix between them is
/// stored, and how wide each annotation frame is. A field the metadata did not
/// give up simply has no row, so a table whose `X` is a representation this
/// version does not know still reports its two counts.
///
/// The column *names* are deliberately not drawn here, unlike the Parquet
/// schema row. A Parquet column appears nowhere else in the output; an `obs`
/// column is a child group of `obs`, which the tree is already printing two
/// levels down. `--json` carries the declared lists either way.
fn anndata_rows(anndata: Option<&AnnData>) -> Vec<String> {
    let Some(anndata) = anndata else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    // Grouped for the same reason a Parquet row count is: a table of 167780
    // cells is a number nobody reads.
    if let Some(count) = anndata.observations {
        rows.push(format!("observations: {}", grouped(count as i64)));
    }
    if let Some(count) = anndata.variables {
        rows.push(format!("variables: {}", grouped(count as i64)));
    }
    if let Some(x) = &anndata.x {
        let mut row = format!("X: {}", x.kind);
        // The representation alone is still worth a row, so a shape or a dtype
        // that could not be read shortens the row rather than removing it.
        if let Some(shape) = &x.shape {
            row.push(' ');
            row.push_str(&format_dims(shape));
        }
        if let Some(dtype) = &x.dtype {
            row.push(' ');
            row.push_str(dtype);
        }
        rows.push(row);
    }
    if let Some(columns) = &anndata.obs_columns {
        rows.push(format!("obs columns: {}", columns.len()));
    }
    if let Some(columns) = &anndata.var_columns {
        rows.push(format!("var columns: {}", columns.len()));
    }
    rows
}

/// The rows a SpatialData table adds about what it annotates.
///
/// Separate from `anndata_rows` because these are separate facts: those come
/// from AnnData's vocabulary, these from SpatialData's, and a table written by
/// one without the other would show one set and not the other.
fn table_rows(spatialdata: Option<&SpatialData>) -> Vec<String> {
    let Some(SpatialData::Table(annotation)) = spatialdata else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    // Capped like the Parquet schema row, and for the same reason: a table
    // over three regions names them, and one over three hundred would run off
    // the side of the terminal.
    if let Some(regions) = &annotation.regions {
        rows.push(format!("annotates: {}", capped(regions).join(", ")));
    }
    if let Some(key) = &annotation.region_key {
        rows.push(format!("region key: {key}"));
    }
    if let Some(key) = &annotation.instance_key {
        rows.push(format!("instance key: {key}"));
    }
    rows
}

/// The first `SHOWN` items of a list, with the rest counted rather than named.
///
/// A terminal is only so wide. `--json` carries every one of these lists whole,
/// which is where a reader who wants them all should look.
fn capped(items: &[String]) -> Vec<String> {
    let mut shown: Vec<String> = items.iter().take(SHOWN).cloned().collect();

    if let Some(rest) = items.len().checked_sub(SHOWN).filter(|rest| *rest > 0) {
        shown.push(format!("... ({rest} more)"));
    }

    shown
}

/// How many items of a list a row names before it starts counting instead.
const SHOWN: usize = 12;

/// The rows an OME-Zarr group adds beneath its own line.
fn ome_rows(ome: Option<&OmeInfo>) -> Vec<String> {
    let Some(ome) = ome else {
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
        // The count is the length of the declared list, so the row and the
        // paths `--validate` checks cannot drift apart.
        if let Some(wells) = wells {
            rows.push(format!("wells: {}", wells.len()));
        }
    }
    rows
}

/// The rows a SpatialData points or shapes element adds beneath its own line,
/// once its Parquet payload has been read.
///
/// Four facts, all four of them read from footers: how many rows the payload
/// holds, how many columns wide it is, how many files it is written across,
/// and what those columns are called and typed.
///
/// A payload that could not be inspected gets the file count and a `?`, and
/// nothing else. That follows the rule an unreadable array field follows --
/// print `?` rather than nothing -- but only once: `rows: ?`, `columns: ?` and
/// `schema: ?` would be four rows saying the same single thing, which is that
/// the payload was not read.
///
/// The schema row is capped. A points payload has a handful of columns and
/// prints whole; a table written with three hundred would run off the side of
/// any terminal, so past a dozen the rest are counted rather than named. The
/// `--json` output carries every column either way, which is where a reader
/// who wants them all should look.
fn parquet_rows(payload: &Payload) -> Vec<String> {
    let parquet = match payload {
        Payload::Absent => return Vec::new(),
        Payload::Unavailable => return vec![String::from("parquet files: ?")],
        Payload::Summary(parquet) => parquet,
    };

    let mut rows = vec![
        format!("rows: {}", grouped(parquet.rows)),
        format!("columns: {}", parquet.columns.len()),
        format!("parquet files: {}", parquet.files),
    ];

    if !parquet.columns.is_empty() {
        let schema: Vec<String> = parquet
            .columns
            .iter()
            .map(|column| format!("{}:{}", column.name, column.kind))
            .collect();

        rows.push(format!("schema: {}", capped(&schema).join(", ")));
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
            dimension_names: None,
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
        // V2 has no dimension-name key, and one is never invented for it.
        dimension_names: None,
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
        dtype: data_type_v3(value.get("data_type")),
        dimension_names: dimension_names_v3(value.get("dimension_names")),
    }
}

/// Read a Zarr V3 `dimension_names` as the names to display, or `None` when
/// there is nothing to show.
///
/// The key is optional, and where it appears it is a list as long as the
/// shape whose entries are each a string or `null`:
///
/// ```json
/// "dimension_names": ["c", null, "y", "x"]
/// ```
///
/// A `null` entry means the dimension has no name. It is kept as an inner
/// `None` and shown as `?`, in its own position, because the alternative --
/// dropping it -- would silently shift every name after it onto the wrong
/// dimension. Nothing is filled in from anywhere else: not from the shape, not
/// from OME-Zarr axes, not from a convention about what four dimensions are
/// usually called.
///
/// Anything that is not a list, and a list with nothing in it, comes back
/// `None`: there is no row to print and the key is left out of `--json`
/// altogether. An entry that is neither a string nor `null` -- a number, an
/// object -- is unreadable rather than absent, and takes the same `?` a
/// `null` does. Either way the array is still an array and the walk goes on;
/// a malformed name costs the reader this one row and nothing more.
fn dimension_names_v3(value: Option<&Value>) -> Option<Vec<Option<String>>> {
    let items = value?.as_array()?;
    if items.is_empty() {
        return None;
    }

    // `as_str` is `None` for a JSON null and for every non-string alike, which
    // is exactly the distinction this row does not need to draw: both are a
    // dimension we cannot name.
    Some(
        items
            .iter()
            .map(|item| item.as_str().map(String::from))
            .collect(),
    )
}

/// Read a Zarr V3 `data_type` as the name to display, or `None` when there is
/// no name to be had.
///
/// V3 spells a data type in two ways. A core type is a bare string --
/// `"uint16"`, `"float32"` -- and comes back as itself. An extension type is
/// an object naming the extension and, usually, configuring it:
///
/// ```json
/// "data_type": {"name": "numpy.datetime64", "configuration": {"unit": "s"}}
/// ```
///
/// Only the `name` is taken. The configuration is what a *reader* needs to
/// decode the values, and this tool decodes nothing; showing it would put a
/// variable-width blob of JSON in a column three characters wide. So an
/// extension array reports `dtype: numpy.datetime64` -- the identity of the
/// type, which is the question a tree can honestly answer -- and the full
/// object stays in the file for anyone who needs it.
///
/// The name is passed through exactly as stored, unchecked against any
/// registry: an unknown extension is displayed, not judged, for the same
/// reason a V2 dtype is printed in whatever NumPy notation it was written in.
///
/// Anything else -- an object with no `name`, a `name` that is not a string, a
/// number, a list -- is a dtype we cannot name, and comes back `None` so the
/// row shows `?`. That is the same degradation an unreadable `shape` gets, and
/// it is deliberately not an error: a malformed dtype costs the reader that
/// one row, never the node or the walk.
fn data_type_v3(value: Option<&Value>) -> Option<String> {
    let value = value?;

    match value {
        Value::String(name) => Some(name.clone()),
        Value::Object(_) => value.get("name")?.as_str().map(String::from),
        _ => None,
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

/// Render dimension names as `c, ?, y, x`.
///
/// A name we do not have -- an explicit `null`, or an entry that was not a
/// string -- becomes `?` in place, so the names that follow it stay on the
/// dimensions they belong to.
fn format_dimension_names(names: &[Option<String>]) -> String {
    let items: Vec<&str> = names
        .iter()
        .map(|name| name.as_deref().unwrap_or("?"))
        .collect();
    items.join(", ")
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

/// Read a list of `{"path": ...}` objects as the paths it declares, or `None`
/// when there is nothing to show.
///
/// Two keys have this shape and both are read here: a `multiscales` entry's
/// `datasets`, which is what the name is for, and a `plate`'s `wells`. They
/// mean quite different things and neither knows about the other, but the
/// reading is one reading, and a second copy of it would be a second place for
/// the `?` rule below to be got wrong.
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
/// array gains a `shards` row between `chunks` and `dtype`, and an array that
/// names its dimensions gains a `dimensions` row after `dtype`; every other
/// array prints exactly the three rows it always has. Whichever row ends up
/// last carries the closing connector. A field we could not read shows as `?`.
fn print_array_meta(out: &mut dyn Write, meta: &ArrayMeta, prefix: &str) -> io::Result<()> {
    // The dimension lists are drawn here rather than kept ready-made, because
    // `--json` wants the same facts as JSON arrays. Rendering late is what
    // lets one reading serve both.
    let shape = meta.shape.as_deref().map(format_dims);
    let chunks = meta.chunks.as_deref().map(format_dims);
    let shards = meta.shards.as_deref().map(format_dims);
    // Joined the way OME axes are, and for the same reason: a list of names
    // reads better bare than in brackets. An unnamed dimension keeps its
    // place as `?` rather than being dropped.
    let names = meta.dimension_names.as_deref().map(format_dimension_names);

    // A `Vec` rather than the fixed array this used to be, because the number
    // of rows is no longer fixed. Everything else about the drawing is the
    // same, including which name the padding is sized to -- "shards:" is the
    // same width as "chunks:".
    let mut rows = vec![("shape:", shape.as_deref()), ("chunks:", chunks.as_deref())];

    if let Some(shards) = shards.as_deref() {
        rows.push(("shards:", Some(shards)));
    }

    rows.push(("dtype:", meta.dtype.as_deref()));

    if let Some(names) = names.as_deref() {
        rows.push(("dimensions:", Some(names)));
    }

    // Taken before the rows are consumed below, so the closing connector lands
    // on whichever row turned out to be last.
    let last = rows.len() - 1;

    // The width of the longest name actually being printed. With the rows an
    // array has always had that is "chunks:" and the output is unchanged; the
    // longer "dimensions:" widens the block only for the arrays that carry
    // one.
    let width = rows
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or_default();

    for (i, (name, value)) in rows.into_iter().enumerate() {
        let connector = if i == last { "└─ " } else { "├─ " };
        let value = value.unwrap_or("?");
        // Padded so the values line up under one another.
        writeln!(out, "{prefix}{connector}{name:<width$} {value}")?;
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
                node["spatialdata"] = json_spatialdata(spatialdata);
            }
            match &meta.parquet {
                Payload::Absent => {}
                Payload::Unavailable => node["parquet"] = Value::Null,
                Payload::Summary(parquet) => node["parquet"] = json_parquet(parquet),
            }
            if let Some(anndata) = &meta.anndata {
                node["anndata"] = json_anndata(anndata);
            }
        }
        NodeKind::Unknown => {}
    }

    // Arrays are leaves here for the same reason they are in the tree: what
    // lies beneath is chunk storage. Their `children` is empty rather than
    // absent, so a reader can walk every node the same way.
    let mut children = Vec::new();
    if !matches!(kind, NodeKind::Array(_)) {
        for name in child_dirs(store, path, &kind, depth)? {
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
/// `shards` and `dimension_names` are the exceptions to that rule, and are
/// left out altogether rather than written as `null`. The two say different
/// things: `null` means the field was looked for and could not be read, which
/// every array has a shape, chunks and a dtype to be. An unsharded array has
/// no shards to miss, and an array that names no dimensions has no names to
/// miss, so those keys are simply not applicable and do not appear. Inside
/// `dimension_names` a `null` is a different thing again -- a dimension the
/// file itself left unnamed.
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

    // `dimension_names` follows the same rule for the same reason: an array
    // that names no dimensions has none to miss. Where it is there, it is the
    // list as stored -- an unnamed dimension is a JSON `null`, keeping its
    // position without being given a name it does not have.
    if let Some(names) = &meta.dimension_names {
        value["dimension_names"] = json!(names);
    }

    value
}

/// What a group is in SpatialData's vocabulary.
///
/// `version` is the container format version, which only a store root records;
/// it is `null` on every element, as it always has been.
///
/// A table adds the three keys describing what it annotates. They appear on a
/// table and on nothing else -- the same rule a plate's counts follow -- and a
/// `null` among them is what the file itself holds: SpatialData writes all
/// three even for a table that annotates nothing.
///
/// `regions` is a list whatever the file wrote, because a single region is
/// stored as a bare string and the difference means nothing -- see `regions`.
fn json_spatialdata(spatialdata: &SpatialData) -> Value {
    let mut value = json!({
        "kind": spatialdata.kind(),
        "version": spatialdata.version(),
    });

    if let SpatialData::Table(annotation) = spatialdata {
        value["regions"] = json!(annotation.regions);
        value["region_key"] = json!(annotation.region_key);
        value["instance_key"] = json!(annotation.instance_key);
    }

    value
}

/// What the AnnData table inside a SpatialData table declares.
///
/// A section of its own beside `spatialdata`, because the two are different
/// vocabularies read from different keys: `spatialdata` is what SpatialData
/// wrote about the elements this table annotates, and this is what AnnData
/// wrote about the table itself.
///
/// `null` follows the rule the rest of this output follows -- a field that was
/// looked for and could not be read -- and every key here applies to every
/// AnnData table, so every key is always present. The two exceptions are the
/// ones that are not applicable rather than unread: `x` has a `dtype` only
/// when it is dense, because a sparse matrix keeps its dtype on an array this
/// program does not open.
///
/// `obs_columns` and `var_columns` are the declared lists whole, however long.
/// The tree caps its counts into a row because a terminal is only so wide, and
/// nothing about JSON is. No column value is here, and none was read.
fn json_anndata(anndata: &AnnData) -> Value {
    let mut value = json!({
        "encoding_version": anndata.encoding_version,
        "observations": anndata.observations,
        "variables": anndata.variables,
        "obs_columns": anndata.obs_columns,
        "var_columns": anndata.var_columns,
        "x": Value::Null,
    });

    if let Some(x) = &anndata.x {
        let mut matrix = json!({
            "kind": x.kind,
            "shape": x.shape,
        });
        if let Some(dtype) = &x.dtype {
            matrix["dtype"] = json!(dtype);
        }
        value["x"] = matrix;
    }

    value
}

/// A SpatialData element's Parquet payload, as the footers gave it.
///
/// A section of its own beside `spatialdata` rather than a field inside it,
/// because it is a different kind of fact: `spatialdata` is what a Zarr
/// metadata file said, and this is what a Parquet file said. A node has this
/// key only when there was a payload, which is the same rule `ome` and `array`
/// follow -- and the key is `null` when there was one and it could not be
/// read, which is the rule every field inside those sections follows.
///
/// `columns` is the count and `schema` the columns themselves, so a reader
/// wanting only the width need not measure the list. The schema is whole here
/// however long it is: the tree caps it because a terminal is only so wide,
/// and nothing about JSON is.
fn json_parquet(parquet: &ParquetSummary) -> Value {
    json!({
        "rows": parquet.rows,
        "columns": parquet.columns.len(),
        "files": parquet.files,
        "schema": parquet
            .columns
            .iter()
            .map(|column| json!({ "name": column.name, "type": column.kind }))
            .collect::<Vec<Value>>(),
    })
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
        if let Some(wells) = wells {
            value["wells"] = json!(wells.len());
        }
    }

    value
}

/// How serious one validation finding is.
///
/// Three answers and no more. The middle one is the load-bearing one: a check
/// this program could not make is not a check that failed, and a store on a
/// server that will not list a directory must not be reported as a broken
/// store. `Warn` is what "I could not look" says; `Error` is reserved for
/// something the metadata declares and the store does not have.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Pass,
    Warn,
    Error,
}

impl Severity {
    /// The word that opens a line of the report.
    fn label(&self) -> &'static str {
        match self {
            Severity::Pass => "PASS",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        }
    }

    /// The same three, for `--json`, which spells its values in lower case.
    fn key(&self) -> &'static str {
        match self {
            Severity::Pass => "pass",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

/// One thing `--validate` looked at, and what it made of it.
///
/// A single struct for every rule, and a `Vec` of them for a whole run. There
/// is no rule type, no registry and no engine here on purpose: the rules are a
/// handful of `if`s over metadata this program already reads, and the moment
/// they become anything more they stop being cheap to read.
///
/// `message` carries its own subject -- "OME dataset path \"0\" exists" -- so a
/// line of the report is legible on its own when it has been grepped out of a
/// thousand others.
struct ValidationFinding {
    severity: Severity,
    /// The node this is about, in the form the report prints: `/` for the
    /// store root, `/images/morphology` for anything below it.
    path: String,
    message: String,
}

/// Build one finding, turning a store path into the form the report prints.
fn finding(severity: Severity, path: &str, message: String) -> ValidationFinding {
    ValidationFinding {
        severity,
        // The store spells its root as the empty string, which would print as
        // nothing at all. A leading slash makes the root `/` and leaves every
        // other path readable as the path it is.
        path: format!("/{path}"),
        message,
    }
}

/// `count` with its noun, pluralised the only way the words here need.
///
/// Used by the summary line and by the payload row, which is the whole reason
/// it is a function: "1 warning, 2 errors" and "1 file, 8 files" are the same
/// rule written once.
fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("{count} {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// Check what a store's metadata declares against what the store has.
///
/// Metadata only, exactly as the tree is: this reads the same files
/// `classify` reads and adds four of AnnData's own -- see `check_anndata` --
/// and it opens no chunk, no Parquet row and no expression value.
///
/// Two passes, because one of the rules crosses from one node to another. The
/// first walks the store and keeps what every node was classified as; the
/// second goes back over that map and asks the questions. A table's `region`
/// names an element that may sit anywhere in the store, and possibly ahead of
/// the table in the walk, so there is no order in which a single streaming
/// pass could answer it.
///
/// The map is keyed by store path, so the second pass runs in path order and
/// the report is the same report every time it is run against the same store.
///
/// `root` is what `run` already classified the root as, passed in rather than
/// worked out again -- the same economy `json_tree` makes.
fn validate(store: &dyn Store, root: NodeKind) -> io::Result<Vec<ValidationFinding>> {
    let mut nodes = BTreeMap::new();
    collect(store, "", root, &mut nodes)?;

    // Gathered once, before the checks, because every table asks the same
    // question of it -- see `spatialdata_elements`.
    let elements = spatialdata_elements(&nodes);

    let mut findings = Vec::new();
    for (path, kind) in &nodes {
        check_node(store, path, kind, &nodes, &elements, &mut findings);
    }

    Ok(findings)
}

/// Walk the store and record what every node is.
///
/// The same walk both renderers make, down to the function that decides which
/// children a node has: `child_dirs` is what drops a points element's Parquet
/// directory, and validating a store must see exactly the nodes printing it
/// would show. `None` for the depth because a validation walk is whole or it
/// is misleading -- a level it never descended into would have every path
/// above it reported as missing, which is why `--depth` and `--validate` are
/// refused together.
///
/// Arrays are leaves here as everywhere else. What lies beneath one is chunk
/// storage, and listing it on a real store means millions of objects.
fn collect(
    store: &dyn Store,
    path: &str,
    kind: NodeKind,
    nodes: &mut BTreeMap<String, NodeKind>,
) -> io::Result<()> {
    let children = match kind {
        NodeKind::Array(_) => Vec::new(),
        _ => child_dirs(store, path, &kind, None)?,
    };

    // Inserted after the children are asked for, because `child_dirs` wants a
    // borrow of the kind and the map wants to own it.
    nodes.insert(String::from(path), kind);

    for name in children {
        let child = child_path(path, &name);
        let child_kind = classify(store, &child);
        collect(store, &child, child_kind, nodes)?;
    }

    Ok(())
}

/// The names of every SpatialData element the store holds.
///
/// SpatialData addresses its elements by name -- `cell_boundaries`, not
/// `shapes/cell_boundaries` -- and that name is the last segment of the node's
/// path. A table is not in this set: a table annotates regions, and a region is
/// an image, a labels, a points or a shapes element.
///
/// A `BTreeSet` of borrowed names rather than owned strings, because the map
/// it is drawn from outlives it and nothing here needs a copy.
fn spatialdata_elements(nodes: &BTreeMap<String, NodeKind>) -> BTreeSet<&str> {
    nodes
        .iter()
        .filter_map(|(path, kind)| {
            let NodeKind::Group(meta) = kind else {
                return None;
            };
            match &meta.spatialdata {
                Some(
                    SpatialData::Image
                    | SpatialData::Labels
                    | SpatialData::Points
                    | SpatialData::Shapes,
                ) => path.rsplit('/').next().filter(|name| !name.is_empty()),
                _ => None,
            }
        })
        .collect()
}

/// Every check that applies to one node, in report order.
fn check_node(
    store: &dyn Store,
    path: &str,
    kind: &NodeKind,
    nodes: &BTreeMap<String, NodeKind>,
    elements: &BTreeSet<&str>,
    findings: &mut Vec<ValidationFinding>,
) {
    // Rule one, the half of it that applies to every node: this program had to
    // be able to read a node's metadata before it can say anything else about
    // it. A store root that says nothing is not a store, and that is an error.
    // A directory further down that says nothing is a directory somebody put
    // there -- a `.git`, a stray export -- and a warning is as much as this can
    // honestly make of it.
    if matches!(kind, NodeKind::Unknown) {
        let severity = match path.is_empty() {
            true => Severity::Error,
            false => Severity::Warn,
        };
        findings.push(finding(
            severity,
            path,
            String::from("no Zarr metadata this tool can read"),
        ));
        return;
    }

    if path.is_empty() {
        findings.push(finding(
            Severity::Pass,
            path,
            String::from("Zarr root metadata is readable"),
        ));
    }

    match kind {
        NodeKind::Array(meta) => check_array(path, meta, findings),
        NodeKind::Group(meta) => {
            if let Some(ome) = &meta.ome {
                check_ome(path, ome, nodes, findings);
            }
            check_spatialdata(store, path, meta, elements, findings);
        }
        // Answered above, and returned from there.
        NodeKind::Unknown => {}
    }
}

/// Rule one, the other half: an array's own dimensions have to agree.
///
/// A shape that could not be read is an error, because every Zarr array has
/// one and this tool needs it. Chunks that could not be read are a warning:
/// only a "regular" chunk grid records a chunk shape, so an array using
/// another one has nothing missing -- it has something this program does not
/// read -- and the comparison is simply not made.
///
/// A V3 `dimension_names` joins the same comparison when the array has one:
/// one name per dimension, counted and not read.
///
/// Nothing here looks at a codec, a fill value or a dtype. This is the
/// dimensionality check the feature asked for and not a Zarr conformance pass.
fn check_array(path: &str, meta: &ArrayMeta, findings: &mut Vec<ValidationFinding>) {
    let Some(shape) = &meta.shape else {
        findings.push(finding(
            Severity::Error,
            path,
            String::from("array declares no readable shape"),
        ));
        return;
    };

    findings.push(match &meta.chunks {
        Some(chunks) if chunks.len() == shape.len() => finding(
            Severity::Pass,
            path,
            format!(
                "array shape and chunks agree on {}",
                plural(shape.len(), "dimension")
            ),
        ),
        Some(chunks) => finding(
            Severity::Error,
            path,
            format!(
                "array shape has {} but its chunks have {}",
                plural(shape.len(), "dimension"),
                plural(chunks.len(), "dimension")
            ),
        ),
        None => finding(
            Severity::Warn,
            path,
            String::from(
                "array declares no readable chunk shape, so its dimensions were not checked",
            ),
        ),
    });

    // A sharded V3 array has a second grid, and it has to describe the same
    // array. Only an array that named the sharding codec has one at all, so
    // every other array is silent here rather than warned about.
    if let Some(shards) = &meta.shards {
        findings.push(match shards.len() == shape.len() {
            true => finding(
                Severity::Pass,
                path,
                format!(
                    "array shape and shards agree on {}",
                    plural(shape.len(), "dimension")
                ),
            ),
            false => finding(
                Severity::Error,
                path,
                format!(
                    "array shape has {} but its shards have {}",
                    plural(shape.len(), "dimension"),
                    plural(shards.len(), "dimension")
                ),
            ),
        });
    }

    // A V3 array may name its own dimensions, and there has to be one name
    // per dimension. Only the count is checked: a `null` entry is a dimension
    // the file deliberately left unnamed, which is a name we do not have and
    // not a dimension that is missing, so it counts like any other. Nothing
    // here looks at what the names say, and nothing compares them with an
    // OME-Zarr `axes` list -- the two are separate metadata and neither is
    // derived from the other.
    //
    // An array that named no dimensions is silent here rather than warned
    // about, exactly as an unsharded array is silent about shards: V2 has no
    // such key and V3 makes it optional, so there is nothing to check.
    if let Some(names) = &meta.dimension_names {
        findings.push(match names.len() == shape.len() {
            true => finding(
                Severity::Pass,
                path,
                format!(
                    "array shape and dimension names agree on {}",
                    plural(shape.len(), "dimension")
                ),
            ),
            false => finding(
                Severity::Error,
                path,
                format!(
                    "array shape has {} but its dimension names cover {}",
                    plural(shape.len(), "dimension"),
                    plural(names.len(), "dimension")
                ),
            ),
        });
    }
}

/// Rules two, three and four: what an OME-Zarr group declares is there.
///
/// A well is checked for nothing. What a well holds is fields of view, and
/// which of them a well must have is an HCS rule this first validation mode
/// deliberately does not know.
fn check_ome(
    path: &str,
    ome: &OmeInfo,
    nodes: &BTreeMap<String, NodeKind>,
    findings: &mut Vec<ValidationFinding>,
) {
    match &ome.kind {
        OmeKind::Image => check_multiscale(path, ome, nodes, findings),
        OmeKind::Plate { wells, .. } => check_plate(path, wells.as_deref(), nodes, findings),
        OmeKind::Well => {}
    }
}

/// Rules two and three: a multiscale's declared levels exist and agree.
///
/// Each `datasets[].path` is resolved against the node map, which is what the
/// walk already read -- no dataset is opened here, and no chunk of one is
/// touched. A path that resolves to a group rather than an array is an error
/// of the same kind as one that resolves to nothing: a pyramid level is an
/// array.
///
/// The dimensions are then compared, and against the axes where the metadata
/// declared any: OME-NGFF gives one axis per dimension, so the count of axes
/// is the count every level should have. With no axes to go on, the first
/// level is the reference -- which still catches a pyramid whose levels
/// disagree, and claims nothing about which of them is right.
///
/// No resolution, scale or downsampling factor is looked at. That is image
/// science, and this is structure.
fn check_multiscale(
    path: &str,
    ome: &OmeInfo,
    nodes: &BTreeMap<String, NodeKind>,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(datasets) = &ome.datasets else {
        findings.push(finding(
            Severity::Warn,
            path,
            String::from("multiscale declares no readable datasets"),
        ));
        return;
    };

    // The levels that resolved to an array with a readable shape, as
    // (declared path, number of dimensions). Only these can be compared.
    let mut levels: Vec<(&str, usize)> = Vec::new();

    for dataset in datasets {
        // `dataset_paths` writes `?` for an entry whose path it could not
        // read, and that is what is being recognised here -- not a level
        // somebody named `?`.
        if dataset == "?" {
            findings.push(finding(
                Severity::Warn,
                path,
                String::from("multiscale declares a dataset with no readable path"),
            ));
            continue;
        }

        let target = child_path(path, dataset);
        findings.push(match nodes.get(&target) {
            Some(NodeKind::Array(meta)) => {
                if let Some(shape) = &meta.shape {
                    levels.push((dataset, shape.len()));
                }
                finding(
                    Severity::Pass,
                    path,
                    format!("OME dataset path {dataset:?} exists"),
                )
            }
            Some(_) => finding(
                Severity::Error,
                path,
                format!("OME dataset path {dataset:?} is not an array"),
            ),
            None => finding(
                Severity::Error,
                path,
                format!("OME dataset path {dataset:?} does not exist"),
            ),
        });
    }

    let Some((first, first_dims)) = levels.first().copied() else {
        return;
    };

    // The axes win where there are any, because they are what the format says
    // a dimension is. The first level answers otherwise.
    let (expected, source) = match &ome.axes {
        Some(axes) => (axes.len(), String::from("the multiscale's axes")),
        None => (first_dims, format!("level {first:?}")),
    };

    let mut disagreed = false;
    for (name, dimensions) in &levels {
        if *dimensions != expected {
            disagreed = true;
            findings.push(finding(
                Severity::Error,
                path,
                format!(
                    "pyramid level {name:?} has {}, but {source} says {expected}",
                    plural(*dimensions, "dimension")
                ),
            ));
        }
    }

    if !disagreed {
        findings.push(finding(
            Severity::Pass,
            path,
            format!(
                "pyramid levels agree with {source} on {}",
                plural(expected, "dimension")
            ),
        ));
    }
}

/// Rule four: a plate's declared wells exist.
///
/// The paths come from the plate's own `wells` list and are resolved against
/// the node map, exactly as a multiscale's datasets are. Nothing else about
/// HCS is checked here: not the row and column names, not the acquisitions,
/// not which fields of view a well holds.
fn check_plate(
    path: &str,
    wells: Option<&[String]>,
    nodes: &BTreeMap<String, NodeKind>,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(wells) = wells else {
        findings.push(finding(
            Severity::Warn,
            path,
            String::from("plate declares no readable wells"),
        ));
        return;
    };

    for well in wells {
        if well == "?" {
            findings.push(finding(
                Severity::Warn,
                path,
                String::from("plate declares a well with no readable path"),
            ));
            continue;
        }

        let target = child_path(path, well);
        findings.push(match nodes.get(&target) {
            Some(NodeKind::Group(_)) => finding(
                Severity::Pass,
                path,
                format!("plate well path {well:?} exists"),
            ),
            Some(_) => finding(
                Severity::Error,
                path,
                format!("plate well path {well:?} is not a group"),
            ),
            None => finding(
                Severity::Error,
                path,
                format!("plate well path {well:?} does not exist"),
            ),
        });
    }
}

/// Rules five, six and seven: what a SpatialData element declares.
///
/// The element's own metadata is the licence for every one of these, exactly
/// as it is for reading the payload in the first place. A group that never
/// said it was a SpatialData element is checked for nothing here.
fn check_spatialdata(
    store: &dyn Store,
    path: &str,
    meta: &GroupMeta,
    elements: &BTreeSet<&str>,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(element) = &meta.spatialdata else {
        return;
    };

    // Rule seven. The three answers are the three `Payload` already
    // distinguishes, and the middle one is why it distinguishes them: a points
    // payload on a server that cannot list a directory is unreadable, not
    // absent and not broken.
    match &meta.parquet {
        Payload::Summary(parquet) => findings.push(finding(
            Severity::Pass,
            path,
            format!(
                "SpatialData {} Parquet payload is readable ({})",
                element.kind(),
                plural(parquet.files, "file")
            ),
        )),
        Payload::Unavailable => findings.push(finding(
            Severity::Warn,
            path,
            format!(
                "SpatialData {} payload metadata unavailable",
                element.kind()
            ),
        )),
        // A payload that is genuinely not there is not a finding. An element
        // is free to have none, and this cannot tell a store that never wrote
        // one from a store that lost it.
        Payload::Absent => {}
    }

    let SpatialData::Table(annotation) = element else {
        return;
    };

    // Rule five. The names are matched against the elements the walk found,
    // and against nothing else: no Parquet column and no `obs` value is read
    // to discover a region name.
    if let Some(regions) = &annotation.regions {
        for region in regions {
            findings.push(if region == "?" {
                finding(
                    Severity::Warn,
                    path,
                    String::from("table names a region this tool could not read"),
                )
            } else if elements.contains(region.as_str()) {
                finding(
                    Severity::Pass,
                    path,
                    format!("table region {region:?} names an existing SpatialData element"),
                )
            } else {
                finding(
                    Severity::Error,
                    path,
                    format!(
                        "table region {region:?} does not name an existing SpatialData element"
                    ),
                )
            });
        }
    }

    check_anndata(store, path, meta.anndata.as_deref(), findings);
}

/// Rule six: an AnnData table's declared axes and its `X` agree.
///
/// The two axis lengths are read again here rather than taken from the
/// `AnnData` the tree prints, and the reason is worth the four extra metadata
/// reads. `AnnData::observations` falls back to `X`'s own shape when the `obs`
/// index cannot be read, which is the right answer to display and a useless
/// one to check: comparing `X` against a number taken from `X` would pass
/// whatever the store held. What this rule needs is the *index* length, which
/// is what `dataframe` returns and only that.
///
/// Nothing is counted and no value is read: the length comes from the index
/// array's declared `shape`, and `X`'s from its own metadata.
fn check_anndata(
    store: &dyn Store,
    path: &str,
    anndata: Option<&AnnData>,
    findings: &mut Vec<ValidationFinding>,
) {
    let Some(anndata) = anndata else {
        return;
    };

    let Some(x) = &anndata.x else {
        findings.push(finding(
            Severity::Warn,
            path,
            String::from("AnnData X could not be read, so the table's dimensions were not checked"),
        ));
        return;
    };
    let shape = x.shape.as_deref();

    for (axis, side, frame, quantity) in [
        (0usize, "rows", "obs", "observations"),
        (1usize, "columns", "var", "variables"),
    ] {
        let length = dataframe(store, &child_path(path, frame)).and_then(|frame| frame.length);
        let declared = dim(shape, axis);

        findings.push(match (length, declared) {
            (Some(length), Some(declared)) if length == declared => finding(
                Severity::Pass,
                path,
                format!("AnnData X {side} match the {length} {quantity} the {frame} index declares"),
            ),
            (Some(length), Some(declared)) => finding(
                Severity::Error,
                path,
                format!(
                    "AnnData X has {declared} {side} but the {frame} index declares {length} {quantity}"
                ),
            ),
            (None, _) => finding(
                Severity::Warn,
                path,
                format!("AnnData {frame} index length unavailable, so the {quantity} were not checked"),
            ),
            (_, None) => finding(
                Severity::Warn,
                path,
                format!("AnnData X declares no usable {side}, so the {quantity} were not checked"),
            ),
        });
    }
}

/// How many findings of each severity there were, in report order.
fn counts(findings: &[ValidationFinding]) -> (usize, usize, usize) {
    let count = |wanted: Severity| {
        findings
            .iter()
            .filter(|found| found.severity == wanted)
            .count()
    };

    (
        count(Severity::Pass),
        count(Severity::Warn),
        count(Severity::Error),
    )
}

/// What the process should exit with once a validation has run.
///
/// Only an `ERROR` changes it. A warning is a check that could not be made,
/// and a tool that failed a build over "I could not list this directory" would
/// be useless on exactly the stores that need looking at most.
fn exit_status(findings: &[ValidationFinding]) -> i32 {
    match findings
        .iter()
        .any(|found| found.severity == Severity::Error)
    {
        true => 2,
        false => 0,
    }
}

/// Print the findings, one per line, and a summary underneath.
///
/// `{:<5}` pads the severity to the width of the longest of the three, so the
/// paths line up. The path and the message are separated by two spaces rather
/// than padded to a column: a store path can be any length, and a column wide
/// enough for the longest would push every short line off the screen.
fn print_validation(out: &mut dyn Write, findings: &[ValidationFinding]) -> io::Result<()> {
    for found in findings {
        writeln!(
            out,
            "{:<5} {}  {}",
            found.severity.label(),
            found.path,
            found.message
        )?;
    }

    let (passed, warnings, errors) = counts(findings);
    writeln!(out)?;
    writeln!(
        out,
        "Validation: {passed} passed, {}, {}",
        plural(warnings, "warning"),
        plural(errors, "error")
    )
}

/// The same findings as JSON.
///
/// One object, not a second format: the findings in the order the report
/// prints them, and the same three counts the summary line carries. A reader
/// that wants only the verdict reads `summary.errors`, which is what the exit
/// status is built from too.
fn json_validation(findings: &[ValidationFinding]) -> Value {
    let (passed, warnings, errors) = counts(findings);

    json!({
        "findings": findings
            .iter()
            .map(|found| json!({
                "severity": found.severity.key(),
                "path": found.path,
                "message": found.message,
            }))
            .collect::<Vec<Value>>(),
        "summary": {
            "passed": passed,
            "warnings": warnings,
            "errors": errors,
        },
    })
}

#[cfg(test)]
mod tests {
    // `tests` is a child module of the crate root, so it can already see the
    // root's private items. This glob import just brings their names into
    // scope so we can call them unqualified.
    use super::*;
    use object_store::PutPayload;
    use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, Int32Type, Int64Type};
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::parser::parse_message_type;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::{TcpListener, TcpStream};
    use std::ops::Range;
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

    /// And for dimension names, where an unnamed dimension keeps its place.
    fn named(names: &Option<Vec<Option<String>>>) -> Option<String> {
        names.as_deref().map(format_dimension_names)
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

    /// One small Zarr V2 store with consolidated metadata, exactly as
    /// zarr-python 3.3.0 wrote it.
    ///
    /// Every per-node file is here as well as the `.zmetadata` that copies
    /// them, because the point of most of these tests is that the per-node
    /// files are never asked for. So are two chunk objects, for the same
    /// reason they are in `FIXTURE`: nothing may ever name them.
    ///
    /// The empty `consolidated_metadata` blocks inside the nested `.zgroup`
    /// entries are zarr-python's own -- its flat on-disk form -- and are
    /// copied here rather than tidied away, so that what is under test is a
    /// real document and not a cleaned-up idea of one.
    const CONSOLIDATED_V2: &[(&str, &str)] = &[
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
            ".zmetadata",
            r#"{"metadata": {".zgroup": {"zarr_format": 2}, ".zattrs": {"note": "root"}, "images/.zattrs": {"multiscales": [{"version": "0.4", "axes": [{"name": "y", "type": "space"}, {"name": "x", "type": "space"}], "datasets": [{"path": "0"}, {"path": "1"}]}]}, "images/.zgroup": {"zarr_format": 2, "consolidated_metadata": {"metadata": {}, "must_understand": false, "kind": "inline"}}, "labels/.zattrs": {}, "labels/.zgroup": {"zarr_format": 2, "consolidated_metadata": {"metadata": {}, "must_understand": false, "kind": "inline"}}, "images/0/.zattrs": {}, "images/0/.zarray": {"shape": [64, 64], "chunks": [32, 32], "dtype": "|u1", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}, "images/1/.zattrs": {}, "images/1/.zarray": {"shape": [32, 32], "chunks": [16, 16], "dtype": "|u1", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}, "labels/mask/.zattrs": {}, "labels/mask/.zarray": {"shape": [8, 8], "chunks": [4, 4], "dtype": "<i4", "fill_value": 0, "order": "C", "filters": null, "dimension_separator": ".", "compressor": {"id": "blosc", "cname": "lz4", "clevel": 5, "shuffle": 1, "blocksize": 0}, "zarr_format": 2}}, "zarr_consolidated_format": 1}"#,
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
        ("images/0/0.0", r#"chunk, not metadata"#),
        ("labels/mask/0.0", r#"chunk, not metadata"#),
    ];

    /// One small Zarr V3 store with inline consolidated metadata, exactly as
    /// zarr-python 3.3.0 wrote it.
    ///
    /// The root `zarr.json` carries the whole hierarchy in one
    /// `consolidated_metadata` block, keyed by path from the root -- `a`,
    /// `a/b`, `a/b/arr` -- with each node's own nested block left empty. That
    /// flat shape is the writer's, not a guess: see `collect_v3`, which reads
    /// a non-empty nested block too.
    const CONSOLIDATED_V3: &[(&str, &str)] = &[
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
        (
            "zarr.json",
            r#"{
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
}"#,
        ),
        ("images/0/c/0/0", r#"chunk, not metadata"#),
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
    /// It answers the four things zarr-tree ever asks of a server and nothing
    /// else: `GET` and `HEAD`, `GET` with a `Range` for a Parquet footer, and
    /// -- when `webdav` is set -- WebDAV `PROPFIND` with `Depth: 1` for
    /// children. With `webdav` off it refuses `PROPFIND` the way an ordinary
    /// static file server does, which is the whole reason it is worth having
    /// one.
    ///
    /// One request per connection, no keep-alive, no concurrency. Enough to
    /// prove the URL mapping, the listing behaviour and the range reads
    /// against the real `object_store` HTTP client; not a web server.
    struct TestServer {
        /// The URL of the store root, as a person would type it.
        url: String,
        /// Every request path the server has seen, in order. What proves a
        /// chunk was never asked for is its absence from here.
        requests: Arc<Mutex<Vec<String>>>,
        /// How many bytes of body the server has actually sent, per path.
        /// What proves a Parquet file was never downloaded whole is this
        /// number set beside the file's own length.
        served: Arc<Mutex<BTreeMap<String, usize>>>,
    }

    impl TestServer {
        fn start(root: &str, objects: &[(&str, &str)], webdav: bool) -> TestServer {
            TestServer::serving(root, objects, &[], webdav)
        }

        /// The same, with binary objects alongside the textual metadata. Only
        /// the Parquet tests need those, and only they pay for the parameter.
        fn serving(
            root: &str,
            objects: &[(&str, &str)],
            binary: &[(&str, Vec<u8>)],
            webdav: bool,
        ) -> TestServer {
            // Port 0 asks the operating system for a free one, so tests running
            // side by side cannot collide.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();

            let mut files: BTreeMap<String, Vec<u8>> = objects
                .iter()
                .map(|(key, body)| (child_path(root, key), body.as_bytes().to_vec()))
                .collect();
            files.extend(
                binary
                    .iter()
                    .map(|(key, body)| (child_path(root, key), body.clone())),
            );

            let requests = Arc::new(Mutex::new(Vec::new()));
            let served: Arc<Mutex<BTreeMap<String, usize>>> = Arc::new(Mutex::new(BTreeMap::new()));
            let log = Arc::clone(&requests);
            let sent = Arc::clone(&served);

            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let Some((method, target, range)) = read_request(&mut stream) else {
                        continue;
                    };
                    log.lock().unwrap().push(target.clone());

                    // Keys are stored without the leading slash the URL has.
                    let key = target.split(['?', '#']).next().unwrap_or("");
                    let key = key.trim_matches('/');

                    let response = match method.as_str() {
                        "GET" | "HEAD" => match files.get(key) {
                            Some(body) => {
                                let head_only = method == "HEAD";
                                if !head_only {
                                    let length = match &range {
                                        Some(range) => range.end - range.start,
                                        None => body.len(),
                                    };
                                    *sent.lock().unwrap().entry(String::from(key)).or_default() +=
                                        length;
                                }
                                body_response(body, range.clone(), head_only)
                            }
                            None => {
                                http_response("404 Not Found", "", method == "HEAD").into_bytes()
                            }
                        },
                        "PROPFIND" if webdav => propfind(&files, key).into_bytes(),
                        // What a static server says. `object_store` hands this
                        // back as a generic failure, which is what
                        // `RemoteStore::diagnose` has to make sense of.
                        _ => http_response("405 Method Not Allowed", "", false).into_bytes(),
                    };

                    let _ = stream.write_all(&response);
                    let _ = stream.flush();
                }
            });

            TestServer {
                url: format!("http://127.0.0.1:{port}/{root}"),
                requests,
                served,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        /// How many bytes of `path`'s body the server has sent altogether.
        fn served(&self, path: &str) -> usize {
            self.served
                .lock()
                .unwrap()
                .get(path)
                .copied()
                .unwrap_or_default()
        }

        /// The store this server's root URL opens, through the same
        /// `open_store` the command line goes through.
        fn open(&self) -> Box<dyn Store> {
            open_store(&self.url).unwrap()
        }
    }

    /// The method, target and requested byte range of one request, or `None`
    /// if the connection died.
    fn read_request(stream: &mut TcpStream) -> Option<(String, String, Option<Range<usize>>)> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if std::io::Read::read(stream, &mut byte).ok()? == 0 {
                return None;
            }
            head.push(byte[0]);
        }

        // "PROPFIND /data/store.zarr HTTP/1.1" -- the first two words are all
        // that is needed there. `Depth` is not read: `list_with_delimiter` is
        // the only caller and it always asks for 1.
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut words = text.split_whitespace();
        let method = String::from(words.next()?);
        let target = String::from(words.next()?);

        Some((method, target, requested_range(&text)))
    }

    /// The byte range a request asked for, if it asked for one.
    ///
    /// Only the `bytes=first-last` form, because that is the only form
    /// `RemoteStore::read_suffix` ever sends: it asks for the size first and
    /// then names both ends.
    fn requested_range(head: &str) -> Option<Range<usize>> {
        let line = head
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("range:"))?;
        let (first, last) = line.split_once('=')?.1.trim().split_once('-')?;
        Some(first.parse().ok()?..last.parse::<usize>().ok()? + 1)
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

    /// A body, whole or in part.
    ///
    /// A range turns this into the `206 Partial Content` with a
    /// `Content-Range` that `object_store` insists on: it refuses a plain
    /// `200` in answer to a range request, and checks the range it is given
    /// against the one it asked for.
    fn body_response(body: &[u8], range: Option<Range<usize>>, head_only: bool) -> Vec<u8> {
        let (status, extra, slice) = match &range {
            Some(range) => {
                let end = range.end.min(body.len());
                let start = range.start.min(end);
                (
                    "206 Partial Content",
                    format!(
                        "Content-Range: bytes {start}-{}/{}\r\n",
                        end.saturating_sub(1),
                        body.len()
                    ),
                    &body[start..end],
                )
            }
            None => ("200 OK", String::new(), body),
        };

        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\n\
             Accept-Ranges: bytes\r\n\
             Last-Modified: Thu, 01 Jan 1970 00:00:00 GMT\r\n\
             {extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
            slice.len()
        )
        .into_bytes();

        if !head_only {
            response.extend_from_slice(slice);
        }
        response
    }

    /// A WebDAV `Depth: 1` answer: the collection itself, then one entry per
    /// immediate child.
    ///
    /// `object_store` drops the collection itself by path length and turns the
    /// rest into common prefixes and objects, so what this has to get right is
    /// the shape, not the filtering.
    fn propfind(files: &BTreeMap<String, Vec<u8>>, prefix: &str) -> String {
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
        for (key, content) in files {
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
                None => body.push_str(&dav_entry(&format!("/{inside}{rest}"), Some(content.len()))),
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
    fn parse_args_reads_the_validate_flag() {
        // Off unless asked for: every command line written before the option
        // existed still means what it meant.
        let Ok(Request::Walk(options)) = parse_args(&args(&["store.zarr"])) else {
            panic!("a lone directory is a valid command line");
        };
        assert!(!options.validate);

        // On its own, and beside `--json`, which it combines with.
        for line in [
            vec!["--validate", "store.zarr"],
            vec!["--json", "--validate", "store.zarr"],
        ] {
            let Ok(Request::Walk(options)) = parse_args(&args(&line)) else {
                panic!("{line:?} is a valid command line");
            };
            assert!(options.validate, "{line:?}");
        }
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
            // The one combination of options that cannot mean both things at
            // once: a validation walk that stopped early would call every node
            // below the limit missing.
            (
                vec!["--validate", "--depth", "1", "store.zarr"],
                "--depth cannot be combined with --validate",
            ),
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
    fn a_v3_object_data_type_is_reported_by_its_extension_name() {
        // The extension form: an object naming the data type, and configuring
        // it. Only the name is displayed -- the configuration is what a reader
        // would need to decode values, and nothing here decodes.
        let value = json!({
            "node_type": "array",
            "shape": [8],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [8] }
            },
            "data_type": {
                "name": "numpy.datetime64",
                "configuration": { "unit": "s", "scale_factor": 1 }
            }
        });

        let meta = array_meta_v3(&value);

        assert_eq!(meta.dtype, Some(String::from("numpy.datetime64")));
        // The rest of the array is read exactly as it would have been with a
        // string dtype: the object form costs nothing else.
        assert_eq!(shown(&meta.shape), Some(String::from("[8]")));
        assert_eq!(shown(&meta.chunks), Some(String::from("[8]")));
    }

    #[test]
    fn an_unknown_v3_object_data_type_is_shown_as_stored() {
        // The name is not checked against any registry, and fields beside it
        // are ignored rather than being a reason to give up. An extension we
        // have never heard of is displayed, not judged.
        let value = json!({
            "node_type": "array",
            "data_type": {
                "name": "example.packed_bits",
                "configuration": { "bits": 3 },
                "must_understand": true,
                "notes": "not a real extension"
            }
        });

        assert_eq!(
            array_meta_v3(&value).dtype,
            Some(String::from("example.packed_bits"))
        );
    }

    #[test]
    fn a_v3_data_type_with_no_usable_name_is_missing_rather_than_fatal() {
        // Three ways to be unnameable: an object with no `name`, a `name` that
        // is not a string, and a value that is neither string nor object. Each
        // costs the dtype row a `?` and nothing else -- the array is still an
        // array, and the shape it did declare still shows.
        for data_type in [
            json!({ "configuration": { "unit": "s" } }),
            json!({ "name": 7 }),
            json!(["uint16"]),
        ] {
            let value = json!({
                "node_type": "array",
                "shape": [4],
                "data_type": data_type
            });

            let meta = array_meta_v3(&value);

            assert_eq!(meta.dtype, None);
            assert_eq!(shown(&meta.shape), Some(String::from("[4]")));
        }
    }

    #[test]
    fn v3_dimension_names_are_read_in_order() {
        let value = json!({
            "node_type": "array",
            "shape": [3, 4, 64, 64],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [1, 1, 64, 64] }
            },
            "data_type": "uint16",
            "dimension_names": ["c", "z", "y", "x"]
        });

        let meta = array_meta_v3(&value);

        assert_eq!(
            named(&meta.dimension_names),
            Some(String::from("c, z, y, x"))
        );
        // Reading them costs the rest of the array nothing.
        assert_eq!(shown(&meta.shape), Some(String::from("[3, 4, 64, 64]")));
        assert_eq!(meta.dtype, Some(String::from("uint16")));
    }

    #[test]
    fn a_null_v3_dimension_name_keeps_its_place_without_being_invented() {
        // V3 lets an entry be null: the dimension is there and is unnamed.
        // Dropping it would move "y" and "x" onto the wrong dimensions, so it
        // stays as an inner `None` and shows as `?`.
        let value = json!({
            "node_type": "array",
            "shape": [3, 4, 64, 64],
            "dimension_names": ["c", null, "y", "x"]
        });

        let names = array_meta_v3(&value)
            .dimension_names
            .expect("a list of dimension names");

        assert_eq!(names.len(), 4);
        assert_eq!(names[1], None);
        assert_eq!(format_dimension_names(&names), "c, ?, y, x");
    }

    #[test]
    fn a_v3_array_that_names_no_dimensions_has_no_names() {
        // The key is optional, so its absence is not a missing value: there is
        // simply no row and no JSON key.
        let value = json!({
            "node_type": "array",
            "shape": [64, 64],
            "data_type": "uint16"
        });

        assert_eq!(array_meta_v3(&value).dimension_names, None);
    }

    #[test]
    fn malformed_v3_dimension_names_cost_the_row_and_nothing_else() {
        // A field that is not a list at all, and an empty one, have nothing to
        // show: no row, no key.
        for dimension_names in [json!("cyx"), json!({ "0": "y" }), json!(7), json!([])] {
            let value = json!({
                "node_type": "array",
                "shape": [64, 64],
                "data_type": "uint16",
                "dimension_names": dimension_names
            });

            let meta = array_meta_v3(&value);

            assert_eq!(meta.dimension_names, None);
            // The array is still an array with everything else intact.
            assert_eq!(shown(&meta.shape), Some(String::from("[64, 64]")));
            assert_eq!(meta.dtype, Some(String::from("uint16")));
        }

        // An entry inside a list that is not a string is a name we cannot
        // read, which is the same `?` a null gets -- the list itself survives.
        let value = json!({
            "node_type": "array",
            "shape": [2, 2],
            "dimension_names": ["y", 7]
        });

        assert_eq!(
            named(&array_meta_v3(&value).dimension_names),
            Some(String::from("y, ?"))
        );
    }

    #[test]
    fn dimension_names_compose_with_an_object_data_type() {
        // The two V3 features are read independently and neither disturbs the
        // other.
        let value = json!({
            "node_type": "array",
            "shape": [8],
            "chunk_grid": {
                "name": "regular",
                "configuration": { "chunk_shape": [8] }
            },
            "data_type": {
                "name": "numpy.datetime64",
                "configuration": { "unit": "s" }
            },
            "dimension_names": ["t"]
        });

        let meta = array_meta_v3(&value);

        assert_eq!(meta.dtype, Some(String::from("numpy.datetime64")));
        assert_eq!(named(&meta.dimension_names), Some(String::from("t")));
    }

    #[test]
    fn a_v2_array_never_names_its_dimensions() {
        // V2 has no such key, and a key that looks like one is not read: this
        // is a V3 field and inventing a V2 spelling for it would be inventing
        // metadata.
        let meta = array_meta_v2(
            r#"{"zarr_format": 2, "shape": [10], "chunks": [10], "dtype": "<u2", "dimension_names": ["x"]}"#,
        );

        assert_eq!(meta.dimension_names, None);
        assert_eq!(shown(&meta.shape), Some(String::from("[10]")));
        assert_eq!(meta.dtype, Some(String::from("<u2")));
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
                parquet: Payload::Absent,
                anndata: None,
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
                parquet: Payload::Absent,
                anndata: None,
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
                parquet: Payload::Absent,
                anndata: None,
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
                parquet: Payload::Absent,
                anndata: None,
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

    #[test]
    fn a_real_zarr_python_v2_zmetadata_is_read_in_place_of_the_store() {
        // The whole store is here, and so is the copy of it in `.zmetadata`.
        // What proves the copy was used is that nothing else was opened.
        let store = memory_store("s3://bucket/store.zarr", "store.zarr", CONSOLIDATED_V2);
        let plain = rendered(&store, "store.zarr", None);
        let consolidated = rendered(consolidate(Box::new(store)).as_ref(), "store.zarr", None);

        // Reading the flat map gives the same tree as walking the store: the
        // same nodes, the same order, the same OME-Zarr tag read out of the
        // same `.zattrs` by the same function.
        assert_eq!(plain, consolidated);
        assert!(
            consolidated.contains("images [group, OME-Zarr 0.4]"),
            "{consolidated}"
        );
        assert!(consolidated.contains("├─ axes: y, x"), "{consolidated}");
        assert!(consolidated.contains("── mask [array]"), "{consolidated}");
        assert!(consolidated.contains("dtype:  <i4"), "{consolidated}");
    }

    #[test]
    fn v2_children_are_derived_from_the_metadata_keys_and_from_nothing_else() {
        // `images/0/.zarray` says there is a node at `images/0`, and by saying
        // so it says there is one at `images` too. The filenames are not nodes
        // and neither are the chunk keys sitting beside them.
        let store = consolidate(Box::new(memory_store(
            "s3://bucket/store.zarr",
            "store.zarr",
            CONSOLIDATED_V2,
        )));

        assert_eq!(
            store.children("").unwrap(),
            vec![String::from("images"), String::from("labels")]
        );
        assert_eq!(
            store.children("images").unwrap(),
            vec![String::from("0"), String::from("1")]
        );
        assert_eq!(
            store.children("labels").unwrap(),
            vec![String::from("mask")]
        );

        // An array is a leaf here as everywhere else: its chunk object is in
        // the fixture, and it is not a child.
        assert_eq!(store.children("images/0").unwrap(), Vec::<String>::new());
        assert_eq!(store.children("labels/mask").unwrap(), Vec::<String>::new());

        // And the documents are keyed by the paths a walk asks for, so the
        // readers above need to know nothing about where they came from.
        assert!(
            store
                .read("images/0/.zarray")
                .unwrap()
                .contains("\"shape\"")
        );
        assert!(store.read(".zgroup").is_some());
        assert!(
            store.read("images/0/0.0").is_none(),
            "a chunk is not metadata"
        );
    }

    #[test]
    fn a_real_zarr_python_v3_consolidated_block_is_read_in_place_of_the_store() {
        let store = memory_store("s3://bucket/store.zarr", "store.zarr", CONSOLIDATED_V3);
        let plain = rendered(&store, "store.zarr", None);
        let consolidated = rendered(consolidate(Box::new(store)).as_ref(), "store.zarr", None);

        assert_eq!(plain, consolidated);

        // Nesting several levels deep, in a document whose keys are flat.
        assert!(consolidated.contains("── a [group]"), "{consolidated}");
        assert!(consolidated.contains("── b [group]"), "{consolidated}");
        assert!(consolidated.contains("── arr [array]"), "{consolidated}");
        assert!(consolidated.contains("dtype:  float32"), "{consolidated}");

        // The V3 attributes readers are handed the same document they would
        // have read off the store, so the OME-Zarr 0.5 tag survives the trip.
        assert!(
            consolidated.contains("images [group, OME-Zarr 0.5]"),
            "{consolidated}"
        );
        assert!(consolidated.contains("├─ axes: y, x"), "{consolidated}");
    }

    #[test]
    fn a_server_that_cannot_list_walks_a_consolidated_store_in_full() {
        // The acceptance case: an ordinary static file server, which answers
        // `GET` and refuses `PROPFIND`. Without consolidation this store could
        // be inspected no further than its root -- see
        // `a_server_that_cannot_list_says_so_rather_than_saying_the_store_is_missing`.
        let server = TestServer::start("data/store.zarr", CONSOLIDATED_V2, false);
        let store = consolidate(server.open());
        let tree = rendered(store.as_ref(), "store.zarr", None);

        assert!(tree.contains("── images [group, OME-Zarr 0.4]"), "{tree}");
        assert!(tree.contains("── mask [array]"), "{tree}");

        // One request for the whole tree, and it is the consolidated document.
        // Everything after it -- eleven nodes' worth of metadata, and the
        // children of every group -- came out of that one response.
        assert_eq!(
            server.requests(),
            vec![String::from("/data/store.zarr/.zmetadata")]
        );
    }

    #[test]
    fn a_consolidated_walk_makes_no_listing_and_reads_no_chunk() {
        // The same over V3, where the document is the root `zarr.json` rather
        // than a file of its own -- so the root read that found it is the root
        // read the walk needed anyway.
        let server = TestServer::start("srv/store.zarr", CONSOLIDATED_V3, false);
        let store = consolidate(server.open());
        let tree = rendered(store.as_ref(), "store.zarr", None);

        assert!(tree.contains("── 0 [array]"), "{tree}");
        assert!(!tree.contains("── c"), "the chunk directory is not a child");

        // Two requests for the whole tree, and the first of them found
        // nothing: V2's `.zmetadata` is looked for first, as V2 is everywhere
        // else here, and a V3 store does not have one. That is one miss per
        // run rather than one per node, and buying it back would mean deciding
        // which Zarr version a store is before reading any of it.
        assert_eq!(
            server.requests(),
            vec![
                String::from("/srv/store.zarr/.zmetadata"),
                String::from("/srv/store.zarr/zarr.json"),
            ]
        );

        // Said the other way round, because a count is easy to read and easy
        // to weaken: no request reached into chunk storage, and none of them
        // was a listing.
        for request in server.requests() {
            assert!(
                !request.contains("/c/"),
                "{request:?} reaches into chunk storage"
            );
        }
    }

    #[test]
    fn depth_limits_a_consolidated_walk_the_way_it_limits_any_other() {
        let store = consolidate(Box::new(memory_store(
            "s3://bucket/store.zarr",
            "store.zarr",
            CONSOLIDATED_V3,
        )));

        assert_eq!(
            rendered(store.as_ref(), "store.zarr", Some(0)),
            "store.zarr [group]\n"
        );

        let one = rendered(store.as_ref(), "store.zarr", Some(1));
        assert!(one.contains("── a [group]"), "{one}");
        assert!(!one.contains("── b [group]"), "{one}");

        let two = rendered(store.as_ref(), "store.zarr", Some(2));
        assert!(two.contains("── b [group]"), "{two}");
        assert!(!two.contains("── arr [array]"), "{two}");
    }

    #[test]
    fn a_consolidation_this_program_does_not_read_leaves_the_store_alone() {
        // Three documents that are not the supported form, and one that is.
        // Each of the three must fall back to reading the store, because a
        // walk that reads the store is always right and only slower.
        let unsupported = [
            // A format version that has never existed.
            json!({"zarr_consolidated_format": 2, "metadata": {".zgroup": {"zarr_format": 2}}}),
            // No version at all.
            json!({"metadata": {".zgroup": {"zarr_format": 2}}}),
            // Not an object where one was promised.
            json!({"zarr_consolidated_format": 1, "metadata": "elsewhere"}),
        ];

        for document in unsupported {
            let mut objects = vec![(".zmetadata", document.to_string())];
            objects.push((".zgroup", String::from(r#"{"zarr_format": 2}"#)));
            objects.push(("A/.zgroup", String::from(r#"{"zarr_format": 2}"#)));
            let objects: Vec<(&str, &str)> = objects
                .iter()
                .map(|(key, body)| (*key, body.as_str()))
                .collect();

            let store = consolidate(Box::new(memory_store(
                "s3://bucket/store.zarr",
                "store.zarr",
                &objects,
            )));
            // The store answered, not the document: `A` is nowhere in it.
            assert_eq!(
                store.children("").unwrap(),
                vec![String::from("A")],
                "{document} should have been ignored"
            );
        }
    }

    #[test]
    fn a_v3_block_demanding_to_be_understood_is_left_alone() {
        // `must_understand: false` is what every document zarr-python writes
        // says, and it means a reader may ignore the block. `true` would be a
        // demand, and the honest answer to one we cannot meet is to read the
        // store instead. So is an unknown `kind`.
        for block in [
            json!({"kind": "inline", "must_understand": true, "metadata": {}}),
            json!({"kind": "elsewhere", "must_understand": false, "metadata": {}}),
        ] {
            let root = json!({
                "zarr_format": 3, "node_type": "group", "attributes": {},
                "consolidated_metadata": block,
            })
            .to_string();
            let objects = [
                ("zarr.json", root.as_str()),
                ("A/zarr.json", r#"{"zarr_format": 3, "node_type": "group"}"#),
            ];

            let store = consolidate(Box::new(memory_store(
                "s3://bucket/store.zarr",
                "store.zarr",
                &objects,
            )));
            assert_eq!(
                store.children("").unwrap(),
                vec![String::from("A")],
                "{block} should have been ignored"
            );
        }
    }

    #[test]
    fn a_v3_block_nested_inside_a_group_is_followed_too() {
        // zarr-python writes the flat form, which the fixtures above cover.
        // A group's block is defined to hold that group's own children,
        // though, so a non-empty nested one is read by the same rule.
        let root = json!({
            "zarr_format": 3, "node_type": "group", "attributes": {},
            "consolidated_metadata": {
                "kind": "inline", "must_understand": false,
                "metadata": {
                    "A": {
                        "zarr_format": 3, "node_type": "group", "attributes": {},
                        "consolidated_metadata": {
                            "kind": "inline", "must_understand": false,
                            "metadata": {
                                "inner": {
                                    "zarr_format": 3, "node_type": "array", "attributes": {},
                                    "shape": [8], "data_type": "uint8",
                                    "chunk_grid": {"name": "regular",
                                                   "configuration": {"chunk_shape": [4]}},
                                },
                            },
                        },
                    },
                },
            },
        })
        .to_string();

        let store = consolidate(Box::new(memory_store(
            "s3://bucket/store.zarr",
            "store.zarr",
            &[("zarr.json", root.as_str())],
        )));

        let tree = rendered(store.as_ref(), "store.zarr", None);
        assert!(tree.contains("── A [group]"), "{tree}");
        assert!(tree.contains("── inner [array]"), "{tree}");
        assert!(tree.contains("shape:  [8]"), "{tree}");
    }

    #[test]
    fn a_node_the_document_never_names_is_still_walked_through() {
        // A `.zgroup` missing from the map, with its children still in it.
        // The group cannot be classified and is `[unknown]` -- the answer an
        // unreadable directory gets -- but the subtree below it stays in the
        // tree rather than disappearing with it.
        let document = json!({
            "zarr_consolidated_format": 1,
            "metadata": {
                ".zgroup": {"zarr_format": 2},
                "A/B/.zarray": {"zarr_format": 2, "shape": [4], "chunks": [4], "dtype": "<i4"},
            },
        })
        .to_string();

        let store = consolidate(Box::new(memory_store(
            "s3://bucket/store.zarr",
            "store.zarr",
            &[(".zmetadata", document.as_str())],
        )));

        let tree = rendered(store.as_ref(), "store.zarr", None);
        assert!(tree.contains("── A [unknown]"), "{tree}");
        assert!(tree.contains("── B [array]"), "{tree}");
    }

    /// One column of a Parquet fixture: what the schema says it is, and what
    /// gets written into it.
    ///
    /// The fixtures are written by the same crate that reads them back, which
    /// is the point: these tests run against real Parquet bytes with a real
    /// footer, not against a stub standing in for one.
    enum Column {
        Double(&'static str),
        Int32(&'static str),
        Int64(&'static str),
        /// A `BYTE_ARRAY` annotated `STRING`, which is what a logical type
        /// looks like in a file and what `column_type` has to prefer.
        Text(&'static str),
        /// A `BYTE_ARRAY` with no annotation -- a geopandas geometry column.
        Binary(&'static str),
    }

    impl Column {
        fn declaration(&self) -> String {
            match self {
                Column::Double(name) => format!("required double {name};"),
                Column::Int32(name) => format!("required int32 {name};"),
                Column::Int64(name) => format!("required int64 {name};"),
                Column::Text(name) => format!("required byte_array {name} (STRING);"),
                Column::Binary(name) => format!("required byte_array {name};"),
            }
        }
    }

    /// One column's worth of values, one per row.
    fn values<T>(rows: usize, of: impl Fn(usize) -> T) -> Vec<T> {
        (0..rows).map(of).collect()
    }

    /// A real Parquet file of `rows` rows, as bytes.
    fn parquet_file(columns: &[Column], rows: usize) -> Vec<u8> {
        let message = format!(
            "message fixture {{ {} }}",
            columns
                .iter()
                .map(Column::declaration)
                .collect::<Vec<String>>()
                .join(" ")
        );

        let schema = std::sync::Arc::new(parse_message_type(&message).unwrap());
        let properties = std::sync::Arc::new(WriterProperties::builder().build());

        let mut bytes: Vec<u8> = Vec::new();
        let mut writer = SerializedFileWriter::new(&mut bytes, schema, properties).unwrap();
        let mut group = writer.next_row_group().unwrap();

        // `next_column` walks the schema in declaration order, so the counter
        // and the column list stay in step.
        let mut index = 0;
        while let Some(mut column) = group.next_column().unwrap() {
            // Every value distinct, so that a big fixture really is big:
            // Parquet would encode a column of one repeated value down to
            // almost nothing, and a test about how little of a large file gets
            // read needs a large file.
            match columns[index] {
                Column::Double(_) => column
                    .typed::<DoubleType>()
                    .write_batch(&values(rows, |row| row as f64 * 1.5), None, None)
                    .map(|_| ()),
                Column::Int32(_) => column
                    .typed::<Int32Type>()
                    .write_batch(&values(rows, |row| row as i32), None, None)
                    .map(|_| ()),
                Column::Int64(_) => column
                    .typed::<Int64Type>()
                    .write_batch(&values(rows, |row| row as i64), None, None)
                    .map(|_| ()),
                Column::Text(_) | Column::Binary(_) => column
                    .typed::<ByteArrayType>()
                    .write_batch(
                        &values(rows, |row| ByteArray::from(format!("value-{row}").as_str())),
                        None,
                        None,
                    )
                    .map(|_| ()),
            }
            .unwrap();

            column.close().unwrap();
            index += 1;
        }

        group.close().unwrap();
        writer.close().unwrap();
        bytes
    }

    /// The Zarr V3 metadata of a SpatialData element of one kind.
    fn element_metadata(encoding: &str) -> String {
        json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "encoding-type": encoding },
        })
        .to_string()
    }

    /// A `LocalStore` that remembers what it was asked for.
    ///
    /// Wrapped rather than replaced, so what is under test is the real store
    /// reading real files -- this only counts. Every method forwards; two of
    /// them keep a note first.
    struct CountingStore {
        inner: LocalStore,
        /// Bytes handed back by `read_suffix`, summed.
        suffix_bytes: Cell<u64>,
        /// Every path `read` was asked for. A Parquet file appearing here
        /// would mean somebody had pulled the whole thing into memory as text.
        reads: Mutex<Vec<String>>,
        /// How many listings were made, of either kind. Remotely each is a
        /// request of its own, which is what makes a summary that needs none
        /// worth proving.
        listings: Cell<usize>,
    }

    impl CountingStore {
        fn new(root: &Path) -> CountingStore {
            CountingStore {
                inner: LocalStore::new(&root.to_string_lossy()),
                suffix_bytes: Cell::new(0),
                reads: Mutex::new(Vec::new()),
                listings: Cell::new(0),
            }
        }
    }

    impl Store for CountingStore {
        fn read(&self, path: &str) -> Option<String> {
            self.reads.lock().unwrap().push(String::from(path));
            self.inner.read(path)
        }

        fn children(&self, path: &str) -> io::Result<Vec<String>> {
            self.listings.set(self.listings.get() + 1);
            self.inner.children(path)
        }

        fn check_root(&self, identified: bool) -> io::Result<()> {
            self.inner.check_root(identified)
        }

        fn files(&self, path: &str) -> io::Result<Vec<String>> {
            self.listings.set(self.listings.get() + 1);
            self.inner.files(path)
        }

        fn read_suffix(&self, path: &str, len: u64) -> Option<Vec<u8>> {
            let bytes = self.inner.read_suffix(path, len)?;
            self.suffix_bytes
                .set(self.suffix_bytes.get() + bytes.len() as u64);
            Some(bytes)
        }
    }

    /// Write one file of bytes into a fixture directory, making the
    /// directories above it.
    fn write_bytes(dir: &Path, path: &str, bytes: &[u8]) {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_parquet_footer_yields_the_rows_columns_and_schema_the_file_declares() {
        let dir = fixture("parquet-footer");
        let columns = [
            Column::Double("x"),
            Column::Double("y"),
            Column::Text("feature_name"),
            Column::Int64("cell_id"),
        ];
        write_bytes(&dir, "shapes.parquet", &parquet_file(&columns, 1234));

        let store = LocalStore::new(&dir.to_string_lossy());
        let metadata = parquet_metadata(&store, "shapes.parquet").unwrap();

        assert_eq!(metadata.file_metadata().num_rows(), 1234);

        let read = schema_columns(metadata.file_metadata().schema());
        let shown: Vec<String> = read
            .iter()
            .map(|column| format!("{}:{}", column.name, column.kind))
            .collect();

        // A `STRING` annotation is reported as `string`, and a `BYTE_ARRAY`
        // without one as `byte_array`: the file's own vocabulary either way.
        assert_eq!(
            shown,
            vec![
                "x:double",
                "y:double",
                "feature_name:string",
                "cell_id:int64"
            ]
        );
    }

    #[test]
    fn a_points_payload_is_a_directory_of_parts_whose_rows_are_summed() {
        let dir = fixture("parquet-points");
        let columns = [
            Column::Double("x"),
            Column::Double("y"),
            Column::Text("gene"),
        ];

        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                ("transcripts/zarr.json", &element_metadata("ngff:points")),
            ],
        );

        // dask writes one file per partition, named `part.N.parquet`. Three
        // partitions of different lengths, so a sum is the only way to arrive
        // at the total.
        for (part, rows) in [(0, 100), (1, 250), (2, 7)] {
            write_bytes(
                &dir,
                &format!("transcripts/points.parquet/part.{part}.parquet"),
                &parquet_file(&columns, rows),
            );
        }

        let store = LocalStore::new(&dir.to_string_lossy());
        let tree = rendered(&store, "store.zarr", None);

        assert!(
            tree.contains("transcripts [group, SpatialData points]"),
            "{tree}"
        );
        assert!(tree.contains("rows: 357"), "{tree}");
        assert!(tree.contains("columns: 3"), "{tree}");
        assert!(tree.contains("parquet files: 3"), "{tree}");
        assert!(
            tree.contains("schema: x:double, y:double, gene:string"),
            "{tree}"
        );
    }

    /// The findings a store yields, as `--validate` prints them.
    ///
    /// Goes through the printer rather than reading the `Vec` directly, so a
    /// test asserting on a line is asserting on the line a reader sees --
    /// severity, path and message, in the order the report puts them.
    fn validated(store: &dyn Store) -> Vec<String> {
        let root = classify(store, "");
        let findings = validate(store, root).expect("a local store can list");

        let mut out: Vec<u8> = Vec::new();
        print_validation(&mut out, &findings).expect("writing to a Vec cannot fail");

        String::from_utf8(out)
            .expect("the report is text")
            .lines()
            .map(String::from)
            .collect()
    }

    /// The findings one V3 array's metadata yields, severity and message.
    ///
    /// Straight off the struct rather than through the printer: these tests
    /// are about which findings one rule makes, and the printed form is the
    /// integration tests' business.
    fn array_findings(value: &Value) -> Vec<(&'static str, String)> {
        let meta = array_meta_v3(value);
        let mut findings = Vec::new();
        check_array("img", &meta, &mut findings);

        findings
            .into_iter()
            .map(|found| (found.severity.label(), found.message))
            .collect()
    }

    /// Only the findings that are about dimension names.
    fn name_findings(value: &Value) -> Vec<(&'static str, String)> {
        array_findings(value)
            .into_iter()
            .filter(|(_, message)| message.contains("dimension names"))
            .collect()
    }

    #[test]
    fn an_array_that_names_no_dimensions_is_not_checked_for_names() {
        // V2 has no such key and V3 makes it optional, so there is nothing to
        // check and nothing to say -- not even a warning. An empty list reads
        // the same way, because that is what `dimension_names_v3` makes of it.
        for value in [
            json!({"zarr_format": 3, "node_type": "array", "shape": [3, 64, 64],
                   "chunk_grid": {"name": "regular",
                                  "configuration": {"chunk_shape": [1, 32, 32]}},
                   "data_type": "uint16"}),
            json!({"zarr_format": 3, "node_type": "array", "shape": [3, 64, 64],
                   "chunk_grid": {"name": "regular",
                                  "configuration": {"chunk_shape": [1, 32, 32]}},
                   "data_type": "uint16", "dimension_names": []}),
        ] {
            assert!(name_findings(&value).is_empty(), "{value}");
            // The rest of rule one still ran.
            assert!(!array_findings(&value).is_empty(), "{value}");
        }
    }

    #[test]
    fn dimension_names_that_match_the_shape_pass_even_where_one_is_null() {
        // A `null` is a dimension the file left unnamed, not a dimension it
        // left out: three entries are three dimensions either way, and the
        // names themselves are never read.
        for names in [json!(["c", "y", "x"]), json!(["c", null, "x"])] {
            let value = json!({"zarr_format": 3, "node_type": "array", "shape": [3, 64, 64],
                               "chunk_grid": {"name": "regular",
                                              "configuration": {"chunk_shape": [1, 32, 32]}},
                               "data_type": "uint16", "dimension_names": names});

            assert_eq!(
                name_findings(&value),
                vec![(
                    "PASS",
                    String::from("array shape and dimension names agree on 3 dimensions")
                )],
                "{value}"
            );
        }
    }

    #[test]
    fn dimension_names_that_do_not_match_the_shape_are_an_error() {
        // Both ways round: too few names and too many are the same fault.
        for (names, count) in [(json!(["c", "y"]), 2), (json!(["c", "z", "y", "x"]), 4)] {
            let value = json!({"zarr_format": 3, "node_type": "array", "shape": [3, 64, 64],
                               "chunk_grid": {"name": "regular",
                                              "configuration": {"chunk_shape": [1, 32, 32]}},
                               "data_type": "uint16", "dimension_names": names});

            assert_eq!(
                name_findings(&value),
                vec![(
                    "ERROR",
                    format!(
                        "array shape has 3 dimensions but its dimension names cover \
                         {count} dimensions"
                    )
                )],
                "{value}"
            );
        }
    }

    #[test]
    fn a_payload_that_cannot_be_inspected_is_a_warning_and_a_readable_one_is_not() {
        // Rule seven, both ways round, in one store. The shapes payload is a
        // real Parquet file and reads; the points payload is a directory of
        // one part that is not Parquet at all, which is what the walk sees on
        // a server that will serve a byte range of nonsense -- or on a
        // half-written store.
        let dir = fixture("validate-parquet");

        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                ("boundaries/zarr.json", &element_metadata("ngff:shapes")),
                ("transcripts/zarr.json", &element_metadata("ngff:points")),
            ],
        );
        write_bytes(
            &dir,
            "boundaries/shapes.parquet",
            &parquet_file(&[Column::Binary("geometry")], 12),
        );
        write_bytes(
            &dir,
            "transcripts/points.parquet/part.0.parquet",
            b"not a parquet file",
        );

        let store = LocalStore::new(&dir.to_string_lossy());
        let report = validated(&store);
        fs::remove_dir_all(&dir).unwrap();

        let has = |needle: &str| report.iter().any(|line| line.contains(needle));

        assert!(
            has("PASS  /boundaries  SpatialData shapes Parquet payload is readable (1 file)"),
            "{report:#?}"
        );
        // A warning, never an error: the payload is there and this could not
        // read it, which is not the same as the store declaring something it
        // does not have.
        assert!(
            has("WARN  /transcripts  SpatialData points payload metadata unavailable"),
            "{report:#?}"
        );
        assert!(
            !report.iter().any(|line| line.starts_with("ERROR")),
            "{report:#?}"
        );
    }

    #[test]
    fn a_shapes_payload_is_one_file_beside_the_element() {
        let dir = fixture("parquet-shapes");

        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                (
                    "cell_boundaries/zarr.json",
                    &element_metadata("ngff:shapes"),
                ),
            ],
        );

        // geopandas writes a GeoDataFrame in one go, so this is a file rather
        // than a directory -- the difference the two element kinds turn on.
        write_bytes(
            &dir,
            "cell_boundaries/shapes.parquet",
            &parquet_file(
                &[Column::Binary("geometry"), Column::Int32("cell_id")],
                167780,
            ),
        );

        let store = LocalStore::new(&dir.to_string_lossy());
        let tree = rendered(&store, "store.zarr", None);

        assert!(
            tree.contains("cell_boundaries [group, SpatialData shapes]"),
            "{tree}"
        );
        assert!(tree.contains("rows: 167,780"), "{tree}");
        assert!(tree.contains("parquet files: 1"), "{tree}");
        assert!(
            tree.contains("schema: geometry:byte_array, cell_id:int32"),
            "{tree}"
        );
    }

    #[test]
    fn only_the_footer_of_a_parquet_file_is_ever_read() {
        let dir = fixture("parquet-footer-only");

        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                ("cells/zarr.json", &element_metadata("ngff:shapes")),
            ],
        );

        // Big enough that reading it whole would show up plainly in the count
        // below, and small enough to write in a test.
        let payload = parquet_file(
            &[Column::Binary("geometry"), Column::Double("radius")],
            200_000,
        );
        let size = payload.len() as u64;
        assert!(size > 4 * PARQUET_TAIL, "the fixture has to dwarf a footer");
        write_bytes(&dir, "cells/shapes.parquet", &payload);

        let store = CountingStore::new(&dir);
        let tree = rendered(&store, "store.zarr", None);
        assert!(tree.contains("rows: 200,000"), "{tree}");

        // One read of the end of the file, and nothing else. Not a fraction of
        // the file: a fixed ceiling, whatever the file's size.
        assert!(
            store.suffix_bytes.get() <= PARQUET_TAIL,
            "read {} bytes of a {size}-byte file",
            store.suffix_bytes.get()
        );

        // And nothing pulled it in as metadata by the back door.
        let reads = store.reads.lock().unwrap();
        assert!(
            !reads.iter().any(|path| path.ends_with(".parquet")),
            "{reads:?}"
        );
    }

    #[test]
    fn a_remote_parquet_footer_is_fetched_as_a_byte_range() {
        let payload = parquet_file(
            &[Column::Binary("geometry"), Column::Int32("cell_id")],
            120_000,
        );
        let size = payload.len();

        let root = json!({ "zarr_format": 3, "node_type": "group", "attributes": {} }).to_string();
        let element = element_metadata("ngff:shapes");

        let server = TestServer::serving(
            "data/store.zarr",
            &[
                ("zarr.json", root.as_str()),
                ("cells/zarr.json", element.as_str()),
            ],
            &[("cells/shapes.parquet", payload)],
            true,
        );

        let store = server.open();
        let tree = rendered(store.as_ref(), "store.zarr", None);
        assert!(tree.contains("rows: 120,000"), "{tree}");

        // The whole point: the server sent a footer, not a file.
        let sent = server.served("data/store.zarr/cells/shapes.parquet");
        assert!(sent > 0, "nothing was fetched at all");
        assert!(
            sent as u64 <= PARQUET_TAIL,
            "the server sent {sent} bytes of a {size}-byte file"
        );
    }

    #[test]
    fn an_element_whose_payload_cannot_be_read_prints_as_it_always_did() {
        let dir = fixture("parquet-missing");

        // Both elements declared, neither with a payload beside it: a store
        // written by a tool that keeps its frames elsewhere, or one copied
        // without them.
        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                ("transcripts/zarr.json", &element_metadata("ngff:points")),
                ("cells/zarr.json", &element_metadata("ngff:shapes")),
            ],
        );

        let store = LocalStore::new(&dir.to_string_lossy());
        let tree = rendered(&store, "store.zarr", None);

        // Still elements, tagged from their metadata exactly as before. The
        // payload is what is missing, not the recognition.
        assert!(tree.contains("cells [group, SpatialData shapes]"), "{tree}");
        assert!(
            tree.contains("transcripts [group, SpatialData points]"),
            "{tree}"
        );
        assert!(!tree.contains("rows:"), "{tree}");
        assert!(!tree.contains("parquet files:"), "{tree}");
    }

    #[test]
    fn a_static_server_reads_a_shapes_payload_but_cannot_find_a_points_one() {
        // No WebDAV, so no listing of any kind -- the server consolidation
        // exists to rescue. A shapes payload is one file at a name we know, so
        // it is read; a points payload is a directory that would have to be
        // listed, so it is not, and its filenames are not guessed at.
        let payload = parquet_file(&[Column::Binary("geometry")], 42);

        let consolidated = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {},
            "consolidated_metadata": {
                "kind": "inline",
                "must_understand": false,
                "metadata": {
                    "cells": {
                        "zarr_format": 3,
                        "node_type": "group",
                        "attributes": { "encoding-type": "ngff:shapes" },
                    },
                    "transcripts": {
                        "zarr_format": 3,
                        "node_type": "group",
                        "attributes": { "encoding-type": "ngff:points" },
                    },
                },
            },
        })
        .to_string();

        let server = TestServer::serving(
            "data/store.zarr",
            &[("zarr.json", consolidated.as_str())],
            &[
                ("cells/shapes.parquet", payload),
                (
                    "transcripts/points.parquet/part.0.parquet",
                    parquet_file(&[Column::Double("x")], 9),
                ),
            ],
            false,
        );

        let store = consolidate(server.open());
        let tree = rendered(store.as_ref(), "store.zarr", None);

        // The consolidated walk is untouched: both elements are there, read
        // out of the document rather than off the server.
        assert!(tree.contains("cells [group, SpatialData shapes]"), "{tree}");
        assert!(
            tree.contains("transcripts [group, SpatialData points]"),
            "{tree}"
        );

        // The shapes payload came off the physical store behind the snapshot.
        assert!(tree.contains("rows: 42"), "{tree}");
        // The points one could not. Its parts are not guessed at, and the row
        // that would have counted them says `?` instead -- an element whose
        // payload could not be reached does not print like one with no
        // payload at all.
        assert!(!tree.contains("rows: 9"), "{tree}");
        assert!(tree.contains("parquet files: ?"), "{tree}");
    }

    #[test]
    fn a_points_payload_that_cannot_be_listed_is_marked_unavailable_not_absent() {
        // The same server that cannot answer a listing, asked about a store
        // holding one points element and one ordinary group. The element is
        // the case this exists for; the group is the control, and must gain
        // nothing at all from it.
        let consolidated = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {},
            "consolidated_metadata": {
                "kind": "inline",
                "must_understand": false,
                "metadata": {
                    "transcripts": {
                        "zarr_format": 3,
                        "node_type": "group",
                        "attributes": { "encoding-type": "ngff:points" },
                    },
                    "other": {
                        "zarr_format": 3,
                        "node_type": "group",
                        "attributes": {},
                    },
                },
            },
        })
        .to_string();

        let server = TestServer::serving(
            "data/store.zarr",
            &[("zarr.json", consolidated.as_str())],
            &[(
                "transcripts/points.parquet/part.0.parquet",
                parquet_file(&[Column::Double("x")], 9),
            )],
            false,
        );

        let store = consolidate(server.open());
        let tree = rendered(store.as_ref(), "store.zarr", None);

        // One marker and one only: the rows, the width and the schema are not
        // separately unknown.
        assert!(tree.contains("parquet files: ?"), "{tree}");
        assert_eq!(tree.matches('?').count(), 1, "{tree}");
        assert!(!tree.contains("rows:"), "{tree}");
        assert!(!tree.contains("schema:"), "{tree}");

        let root = classify(store.as_ref(), "");
        let value = json_tree(store.as_ref(), "", "store.zarr", root, None).unwrap();

        // `null` says the section was there and could not be read, which is
        // the rule every field inside a section already follows. The ordinary
        // group has no such section, so it has no key.
        let children = value["children"].as_array().unwrap();
        let element = children
            .iter()
            .find(|child| child["name"] == "transcripts")
            .unwrap();
        let group = children
            .iter()
            .find(|child| child["name"] == "other")
            .unwrap();

        assert_eq!(element["parquet"], Value::Null);
        assert!(element.get("parquet").is_some());
        assert!(group.get("parquet").is_none());
    }

    #[test]
    fn a_consolidated_walk_still_reads_payloads_off_the_store_behind_it() {
        let dir = fixture("parquet-consolidated");

        // A `.zmetadata` that names both nodes, so every Zarr read comes out
        // of the snapshot. The Parquet file is not in it and cannot be: no
        // consolidated document has ever carried one.
        let document = json!({
            "zarr_consolidated_format": 1,
            "metadata": {
                ".zgroup": { "zarr_format": 2 },
                "cells/.zgroup": { "zarr_format": 2 },
                "cells/.zattrs": { "encoding-type": "ngff:shapes" },
            },
        })
        .to_string();

        write_fixture(&dir, &[(".zmetadata", &document)]);
        write_bytes(
            &dir,
            "cells/shapes.parquet",
            &parquet_file(&[Column::Binary("geometry"), Column::Double("radius")], 88),
        );

        let store = consolidate(Box::new(LocalStore::new(&dir.to_string_lossy())));
        let tree = rendered(store.as_ref(), "store.zarr", None);

        // The hierarchy came out of the document -- there is no `cells/.zgroup`
        // on disk at all -- and the payload came off the disk behind it.
        assert!(tree.contains("cells [group, SpatialData shapes]"), "{tree}");
        assert!(tree.contains("rows: 88"), "{tree}");
        assert!(
            tree.contains("schema: geometry:byte_array, radius:double"),
            "{tree}"
        );
    }

    #[test]
    fn the_json_carries_the_whole_schema_where_the_tree_abbreviates_it() {
        let dir = fixture("parquet-json");

        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                ("cells/zarr.json", &element_metadata("ngff:shapes")),
            ],
        );
        write_bytes(
            &dir,
            "cells/shapes.parquet",
            &parquet_file(&[Column::Binary("geometry"), Column::Int32("cell_id")], 5),
        );

        let store = LocalStore::new(&dir.to_string_lossy());
        let root = classify(&store, "");
        let value = json_tree(&store, "", "store.zarr", root, None).unwrap();

        let element = &value["children"][0];
        assert_eq!(element["spatialdata"]["kind"], "shapes");
        assert_eq!(element["parquet"]["rows"], 5);
        assert_eq!(element["parquet"]["columns"], 2);
        assert_eq!(element["parquet"]["files"], 1);
        assert_eq!(
            element["parquet"]["schema"],
            json!([
                { "name": "geometry", "type": "byte_array" },
                { "name": "cell_id", "type": "int32" },
            ])
        );
    }

    #[test]
    fn a_long_schema_is_cut_short_in_the_tree_and_counted() {
        let columns: Vec<ParquetColumn> = (0..20)
            .map(|index| ParquetColumn {
                name: format!("c{index}"),
                kind: String::from("double"),
            })
            .collect();

        let rows = parquet_rows(&Payload::Summary(ParquetSummary {
            rows: 4_825_319,
            files: 12,
            columns,
        }));

        assert_eq!(rows[0], "rows: 4,825,319");
        assert_eq!(rows[1], "columns: 20");
        assert_eq!(rows[2], "parquet files: 12");
        // Twelve named, the other eight counted.
        assert!(rows[3].starts_with("schema: c0:double, "), "{}", rows[3]);
        assert!(rows[3].ends_with("c11:double, ... (8 more)"), "{}", rows[3]);
    }

    #[test]
    fn a_parquet_file_elsewhere_in_a_store_is_not_a_payload() {
        let dir = fixture("parquet-not-an-element");

        // An ordinary group whose directory happens to be called `points`,
        // with a file that happens to be called `points.parquet`. Neither name
        // means anything: no metadata called this an element.
        write_fixture(
            &dir,
            &[
                (
                    "zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
                (
                    "points/zarr.json",
                    &json!({ "zarr_format": 3, "node_type": "group", "attributes": {} })
                        .to_string(),
                ),
            ],
        );
        write_bytes(
            &dir,
            "points/points.parquet/part.0.parquet",
            &parquet_file(&[Column::Double("x")], 3),
        );

        let store = CountingStore::new(&dir);
        let tree = rendered(&store, "store.zarr", None);

        assert!(tree.contains("points [group]"), "{tree}");
        assert!(!tree.contains("rows:"), "{tree}");
        // Not read, not opened, not even measured.
        assert_eq!(store.suffix_bytes.get(), 0);
    }

    // ----------------------------------------------------------------------
    // AnnData tables inside a SpatialData store.
    //
    // The numbers below are the ones the Xenium 1.0 replicate really holds --
    // 167780 cells over 313 genes, a CSR `X`, eight `obs` columns and three
    // `var` columns -- so a test that changes one has to mean it.
    // ----------------------------------------------------------------------

    /// The Zarr V3 metadata of a table group, as SpatialData's writer writes
    /// it: AnnData's two keys, SpatialData's element kind, and the three that
    /// say what the table annotates.
    fn table_metadata(region: Value) -> String {
        json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "anndata",
                "encoding-version": "0.1.0",
                "spatialdata-encoding-type": "ngff:regions_table",
                "region": region,
                "region_key": "region",
                "instance_key": "cell_id",
                "version": "0.2",
            },
        })
        .to_string()
    }

    /// The Zarr V3 metadata of an AnnData dataframe group.
    fn dataframe_metadata(columns: &[&str]) -> String {
        json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "column-order": columns,
                "_index": "_index",
                "encoding-type": "dataframe",
                "encoding-version": "0.2.0",
            },
        })
        .to_string()
    }

    /// The Zarr V3 metadata of a one-dimensional string array, which is what
    /// an AnnData dataframe index is.
    fn index_metadata(length: usize) -> String {
        json!({
            "zarr_format": 3,
            "node_type": "array",
            "shape": [length],
            "data_type": "string",
            "chunk_grid": { "name": "regular", "configuration": { "chunk_shape": [length] } },
            "attributes": { "encoding-type": "string-array", "encoding-version": "0.2.0" },
        })
        .to_string()
    }

    /// A whole Zarr V3 SpatialData table on disk, with `X` written as given.
    ///
    /// Every fixture below differs only in `X` and in what the table says it
    /// annotates, so those are the two things passed in. The chunk files are
    /// there to be left alone: nothing in this program should ever open one.
    fn write_table(dir: &Path, region: Value, x: &[(&str, &str)]) {
        let table = table_metadata(region);
        let obs = dataframe_metadata(&[
            "cell_id",
            "transcript_counts",
            "control_probe_counts",
            "control_codeword_counts",
            "total_counts",
            "cell_area",
            "nucleus_area",
            "region",
        ]);
        let var = dataframe_metadata(&["gene_ids", "feature_types", "genome"]);
        let obs_index = index_metadata(167780);
        let var_index = index_metadata(313);

        let mut objects = vec![
            ("zarr.json", table.as_str()),
            ("obs/zarr.json", obs.as_str()),
            ("obs/_index/zarr.json", obs_index.as_str()),
            ("obs/_index/c/0", "index values, and never read"),
            ("var/zarr.json", var.as_str()),
            ("var/_index/zarr.json", var_index.as_str()),
            ("var/_index/c/0", "index values, and never read"),
        ];
        objects.extend_from_slice(x);

        write_fixture(dir, &objects);
    }

    /// A sparse `X`: the group that declares the representation and the shape,
    /// and the three arrays beneath it that this program never opens.
    fn sparse_x(encoding: &str) -> [(&'static str, String); 4] {
        let group = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "shape": [167780, 313],
                "encoding-type": encoding,
                "encoding-version": "0.1.0",
            },
        })
        .to_string();

        let array = json!({
            "zarr_format": 3,
            "node_type": "array",
            "shape": [23409569],
            "data_type": "float32",
            "chunk_grid": { "name": "regular", "configuration": { "chunk_shape": [131072] } },
        })
        .to_string();

        [
            ("X/zarr.json", group),
            ("X/data/zarr.json", array.clone()),
            (
                "X/data/c/0",
                String::from("expression values, and never read"),
            ),
            ("X/indptr/zarr.json", array),
        ]
    }

    /// The same as a slice of string pairs, ready for `write_table`.
    fn as_objects<'a>(objects: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
        objects
            .iter()
            .map(|(path, body)| (*path, body.as_str()))
            .collect()
    }

    /// The metadata rows a table draws, for a fixture on disk.
    fn table_rows_of(dir: &Path) -> Vec<String> {
        group_rows(&classify_dir(dir))
    }

    #[test]
    fn a_table_reports_the_shape_and_annotation_its_metadata_declares() {
        let dir = fixture("anndata-csr");
        let x = sparse_x("csr_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // Eight rows, and not one of them was counted. The two lengths come
        // from the index arrays' declared shapes, the two widths from the
        // declared `column-order`, and the last three from the table group's
        // own attributes.
        assert_eq!(
            rows,
            vec![
                "observations: 167,780",
                "variables: 313",
                "X: csr [167780, 313]",
                "obs columns: 8",
                "var columns: 3",
                "annotates: cell_circles",
                "region key: region",
                "instance key: cell_id",
            ]
        );
    }

    #[test]
    fn a_csc_matrix_is_reported_as_csc() {
        // The other sparse representation AnnData writes. Matched exactly, so
        // it is a row of its own rather than a csr misreported.
        let dir = fixture("anndata-csc");
        let x = sparse_x("csc_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert!(
            rows.contains(&String::from("X: csc [167780, 313]")),
            "{rows:?}"
        );
    }

    #[test]
    fn a_dense_x_reports_the_shape_and_dtype_of_the_array_it_is() {
        let dir = fixture("anndata-dense");
        let dense = json!({
            "zarr_format": 3,
            "node_type": "array",
            "shape": [167780, 313],
            "data_type": "float32",
            "chunk_grid": { "name": "regular", "configuration": { "chunk_shape": [4096, 313] } },
            "attributes": { "encoding-type": "array", "encoding-version": "0.2.0" },
        })
        .to_string();

        write_table(
            &dir,
            json!("cell_circles"),
            &[
                ("X/zarr.json", dense.as_str()),
                ("X/c/0/0", "expression values, and never read"),
            ],
        );

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // A dense `X` is a Zarr array, so it has a dtype of its own to show --
        // the one thing a sparse row cannot say, because the dtype is on an
        // array inside the group and that array is not opened.
        assert!(
            rows.contains(&String::from("X: dense [167780, 313] float32")),
            "{rows:?}"
        );
    }

    #[test]
    fn a_table_over_several_regions_names_them_all() {
        // mibitof: one table annotating three label images, written as a list.
        // A single region is a bare string, and both mean the same thing.
        let dir = fixture("anndata-regions");
        let x = sparse_x("csr_matrix");
        write_table(
            &dir,
            json!(["point8_labels", "point16_labels", "point23_labels"]),
            &as_objects(&x),
        );

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert!(
            rows.contains(&String::from(
                "annotates: point8_labels, point16_labels, point23_labels"
            )),
            "{rows:?}"
        );
    }

    #[test]
    fn a_table_annotating_nothing_draws_no_annotation_rows() {
        // SpatialData writes all three keys even when there is nothing to put
        // in them, so this is a null rather than a missing key -- and a null
        // is not something to print.
        let dir = fixture("anndata-no-region");
        let x = sparse_x("csr_matrix");
        let table = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": {
                "encoding-type": "anndata",
                "spatialdata-encoding-type": "ngff:regions_table",
                "region": Value::Null,
                "region_key": Value::Null,
                "instance_key": Value::Null,
            },
        })
        .to_string();

        let mut objects = vec![("zarr.json", table.as_str())];
        let x = as_objects(&x);
        objects.extend_from_slice(&x);
        write_fixture(&dir, &objects);

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // The table is still a table, and still says how big it is. It just
        // has nothing to say about what it annotates.
        assert_eq!(
            rows,
            vec![
                "observations: 167,780",
                "variables: 313",
                "X: csr [167780, 313]"
            ]
        );
    }

    #[test]
    fn a_table_whose_var_is_unreadable_still_reports_what_obs_declared() {
        // Every field falls out on its own. Here `var` is missing altogether
        // and `X` declares a shape that is not a pair of numbers, so the
        // variable count has nowhere left to come from -- and the rows that
        // could be read are drawn anyway.
        let dir = fixture("anndata-degraded");
        let obs = dataframe_metadata(&["cell_id"]);
        let obs_index = index_metadata(3309);
        let x = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "shape": "wide", "encoding-type": "csr_matrix" },
        })
        .to_string();
        let table = table_metadata(json!("cells"));

        write_fixture(
            &dir,
            &[
                ("zarr.json", table.as_str()),
                ("obs/zarr.json", obs.as_str()),
                ("obs/_index/zarr.json", obs_index.as_str()),
                ("X/zarr.json", x.as_str()),
            ],
        );

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        // No `variables` row and no `var columns` row, rather than a zero or a
        // `?`; the representation still shows, because it was readable even
        // though the shape beside it was not.
        assert_eq!(
            rows,
            vec![
                "observations: 3,309",
                "X: csr",
                "obs columns: 1",
                "annotates: cells",
                "region key: region",
                "instance key: cell_id",
            ]
        );
    }

    #[test]
    fn an_index_that_cannot_be_read_falls_back_to_the_shape_x_declares() {
        // The index array is the preferred source because it is the
        // dataframe's own account of its length. When there is none to read,
        // `X`'s declared shape is already in hand and says the same thing.
        let dir = fixture("anndata-fallback");
        let x = sparse_x("csr_matrix");
        let empty = json!({
            "zarr_format": 3,
            "node_type": "group",
            "attributes": { "encoding-type": "dataframe" },
        })
        .to_string();
        let table = table_metadata(json!("cell_circles"));

        let mut objects = vec![
            ("zarr.json", table.as_str()),
            ("obs/zarr.json", empty.as_str()),
            ("var/zarr.json", empty.as_str()),
        ];
        let x = as_objects(&x);
        objects.extend_from_slice(&x);
        write_fixture(&dir, &objects);

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert!(
            rows.contains(&String::from("observations: 167,780")),
            "{rows:?}"
        );
        assert!(rows.contains(&String::from("variables: 313")), "{rows:?}");
        // No `column-order` was declared, so no width is reported. Neither
        // dataframe's children were listed to make one up.
        assert!(!rows.iter().any(|row| row.contains("columns")), "{rows:?}");
    }

    #[test]
    fn summarising_a_table_reads_metadata_and_never_a_chunk_or_a_listing() {
        let dir = fixture("anndata-reads");
        let x = sparse_x("csr_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        let store = CountingStore::new(&dir);
        let summary = anndata_summary(
            &store,
            "",
            Some(&json!({ "encoding-version": "0.1.0" })),
            Some(&SpatialData::Table(table_annotation(&json!({})))),
        )
        .expect("a table element should be summarised");

        let reads = store.reads.lock().unwrap().clone();

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(summary.observations, Some(167780));
        assert_eq!(summary.variables, Some(313));

        // Five nodes, one file each, and every one of them named by a metadata
        // file rather than found by looking around: `obs` and `var`, the index
        // array each of them named, and `X`.
        assert_eq!(
            reads,
            vec![
                "obs/zarr.json",
                "obs/_index/zarr.json",
                "var/zarr.json",
                "var/_index/zarr.json",
                "X/zarr.json",
            ]
        );

        // Said again the other way round, because a list is easy to weaken.
        // Nothing under an array was opened, and nothing was listed -- which
        // remotely is what keeps this to five requests wherever the store is.
        for path in &reads {
            assert!(!path.contains("/c/"), "{path:?} reaches into chunk storage");
        }
        assert_eq!(store.listings.get(), 0);
    }

    #[test]
    fn an_ordinary_group_holding_x_obs_and_var_is_not_a_table() {
        // The recognition rule is the SpatialData marker and nothing else. A
        // group whose children are called `X`, `obs` and `var` -- or which is
        // itself called `table` -- says nothing about itself by doing so.
        let dir = fixture("anndata-lookalike");
        let plain = json!({ "zarr_format": 3, "node_type": "group" }).to_string();
        let obs = dataframe_metadata(&["cell_id"]);
        let obs_index = index_metadata(3309);

        write_fixture(
            &dir,
            &[
                ("zarr.json", plain.as_str()),
                ("obs/zarr.json", obs.as_str()),
                ("obs/_index/zarr.json", obs_index.as_str()),
            ],
        );

        let meta = group_meta(&dir);
        let rows = group_rows(&NodeKind::Group(GroupMeta {
            ome: meta.ome,
            spatialdata: meta.spatialdata,
            parquet: meta.parquet,
            anndata: meta.anndata,
        }));

        fs::remove_dir_all(&dir).unwrap();

        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn a_zarr_v2_table_is_read_from_its_split_metadata_files() {
        // V2 keeps attributes in `.zattrs` and an array's shape in `.zarray`,
        // and puts the same AnnData and SpatialData keys in them. The reader
        // above is one reader: only where the bytes are differs.
        let dir = fixture("anndata-v2");
        let group = r#"{"zarr_format": 2}"#;

        write_fixture(
            &dir,
            &[
                (".zgroup", group),
                (
                    ".zattrs",
                    &json!({
                        "encoding-type": "anndata",
                        "encoding-version": "0.1.0",
                        "spatialdata-encoding-type": "ngff:regions_table",
                        "region": "cells",
                        "region_key": "region",
                        "instance_key": "cell_id",
                    })
                    .to_string(),
                ),
                ("obs/.zgroup", group),
                (
                    "obs/.zattrs",
                    &json!({
                        "column-order": ["cell_id", "region"],
                        "_index": "_index",
                        "encoding-type": "dataframe",
                    })
                    .to_string(),
                ),
                (
                    "obs/_index/.zarray",
                    &json!({ "zarr_format": 2, "shape": [2389], "chunks": [2389], "dtype": "|O" })
                        .to_string(),
                ),
                ("var/.zgroup", group),
                (
                    "var/.zattrs",
                    &json!({ "column-order": [], "_index": "_index", "encoding-type": "dataframe" })
                        .to_string(),
                ),
                (
                    "var/_index/.zarray",
                    &json!({ "zarr_format": 2, "shape": [268], "chunks": [268], "dtype": "|O" })
                        .to_string(),
                ),
                ("X/.zgroup", group),
                (
                    "X/.zattrs",
                    &json!({ "shape": [2389, 268], "encoding-type": "csr_matrix" }).to_string(),
                ),
            ],
        );

        let rows = table_rows_of(&dir);

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            rows,
            vec![
                "observations: 2,389",
                "variables: 268",
                "X: csr [2389, 268]",
                "obs columns: 2",
                "var columns: 0",
                "annotates: cells",
                "region key: region",
                "instance key: cell_id",
            ]
        );
    }

    #[test]
    fn the_summary_rows_sit_above_the_anndata_subtree_rather_than_replacing_it() {
        // The rows are a summary, not a substitute. Everything AnnData wrote
        // is still a group in the tree, and still walked into.
        let dir = fixture("anndata-subtree");
        let x = sparse_x("csr_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        let store = LocalStore::new(&dir.to_string_lossy());
        let tree = rendered(&store, "table", Some(1));

        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            tree.lines().collect::<Vec<&str>>(),
            vec![
                "table [group, SpatialData table]",
                "├─ observations: 167,780",
                "├─ variables: 313",
                "├─ X: csr [167780, 313]",
                "├─ obs columns: 8",
                "├─ var columns: 3",
                "├─ annotates: cell_circles",
                "├─ region key: region",
                "├─ instance key: cell_id",
                "├── X [group]",
                "├── obs [group]",
                "└── var [group]",
            ]
        );
    }

    #[test]
    fn json_carries_the_declared_columns_the_tree_only_counted() {
        let dir = fixture("anndata-json");
        let x = sparse_x("csr_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        let store = LocalStore::new(&dir.to_string_lossy());
        let kind = classify(&store, "");
        let tree = json_tree(&store, "", "table", kind, Some(0)).unwrap();

        fs::remove_dir_all(&dir).unwrap();

        // The AnnData facts and the SpatialData ones stay in separate objects,
        // because they were read from separate vocabularies.
        assert_eq!(
            tree["anndata"],
            json!({
                "encoding_version": "0.1.0",
                "observations": 167780,
                "variables": 313,
                "x": { "kind": "csr", "shape": [167780, 313] },
                "obs_columns": [
                    "cell_id",
                    "transcript_counts",
                    "control_probe_counts",
                    "control_codeword_counts",
                    "total_counts",
                    "cell_area",
                    "nucleus_area",
                    "region",
                ],
                "var_columns": ["gene_ids", "feature_types", "genome"],
            })
        );
        assert_eq!(
            tree["spatialdata"],
            json!({
                "kind": "table",
                "version": Value::Null,
                "regions": ["cell_circles"],
                "region_key": "region",
                "instance_key": "cell_id",
            })
        );
    }

    #[test]
    fn a_consolidated_table_is_summarised_from_the_snapshot_alone() {
        // A static server answers `GET` and nothing else, so without the
        // consolidated document there is no way past the root. With it, the
        // five metadata files the summary wants are already in hand -- and the
        // server is asked for none of them.
        let dir = fixture("anndata-consolidated");
        let x = sparse_x("csr_matrix");
        write_table(&dir, json!("cell_circles"), &as_objects(&x));

        // The snapshot, built from the very files just written, so the two
        // cannot drift apart.
        let store = LocalStore::new(&dir.to_string_lossy());
        let mut metadata = serde_json::Map::new();
        for path in [
            "obs/zarr.json",
            "obs/_index/zarr.json",
            "var/zarr.json",
            "var/_index/zarr.json",
            "X/zarr.json",
        ] {
            let node: Value = serde_json::from_str(&store.read(path).unwrap()).unwrap();
            let name = path.trim_end_matches("/zarr.json");
            metadata.insert(String::from(name), node);
        }

        let mut root: Value = serde_json::from_str(&store.read("zarr.json").unwrap()).unwrap();
        root["consolidated_metadata"] = json!({
            "kind": "inline",
            "must_understand": false,
            "metadata": metadata,
        });
        let root = root.to_string();

        fs::remove_dir_all(&dir).unwrap();

        let server = TestServer::start("srv/store.zarr", &[("zarr.json", root.as_str())], false);
        let store = consolidate(server.open());
        let tree = rendered(store.as_ref(), "table", None);

        assert!(tree.contains("├─ observations: 167,780"), "{tree}");
        assert!(tree.contains("├─ X: csr [167780, 313]"), "{tree}");
        assert!(tree.contains("├─ obs columns: 8"), "{tree}");
        assert!(tree.contains("└── var [group]"), "{tree}");

        // Two requests for the whole store, summary included: the `.zmetadata`
        // that a V3 store does not have, and the root that holds everything.
        assert_eq!(
            server.requests(),
            vec![
                String::from("/srv/store.zarr/.zmetadata"),
                String::from("/srv/store.zarr/zarr.json"),
            ]
        );
    }
}
