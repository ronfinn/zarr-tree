use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

fn main() {
    // args[0] is the program itself, so we expect exactly two entries.
    let args: Vec<String> = env::args().collect();
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

    println!("{}/", args[1].trim_end_matches('/'));
    if let Err(e) = print_tree(root, "") {
        eprintln!("error: {e}");
        process::exit(1);
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
        println!("{prefix}{connector}{name}/");

        // Children of the last entry need no vertical bar above them.
        let child_prefix = if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };
        print_tree(path, &child_prefix)?;
    }

    Ok(())
}
