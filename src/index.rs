//! Folder scanning and lightweight metadata extraction — title/tune discovery *without*
//! rendering, so opening a folder is instant.

use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;

use crate::model::Format;

/// One browsable source file.
pub struct FileEntry {
    pub path: PathBuf,
    pub format: Format,
    /// File name, shown in the browser.
    pub title: String,
    /// LilyPond `\header { title }` — searchable, since the title lives only in the content.
    /// `None` for ABC, whose titles are the per-tune `T:` headers below.
    pub header_title: Option<String>,
    /// ABC files are multi-tune containers; one entry per `X:` tune. Empty for LilyPond.
    pub tunes: Vec<Tune>,
}

pub struct Tune {
    /// The ABC `X:` reference number (used for `abcm2ps -e` selection).
    pub number: u32,
    pub title: String,
}

/// Recursively scan `root` for `.ly`/`.abc` files, sorted by path.
pub fn scan(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Some(format) = Format::from_path(&path) else {
            continue;
        };
        // Read leniently (Latin-1 / non-UTF8 tolerant) so we can index tunes and search titles.
        let content =
            String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).into_owned();
        let (header_title, tunes) = match format {
            Format::Abc => (None, parse_abc_tunes(&content)),
            Format::LilyPond => (lilypond_title(&content), Vec::new()),
        };
        let title = file_name(&path);
        out.push(FileEntry {
            path,
            format,
            title,
            header_title,
            tunes,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Extract `title = "..."` from a LilyPond `\header` block, for search. `None` if absent.
fn lilypond_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(pos) = line.find("title") {
            let after = &line[pos + "title".len()..];
            if let Some(q1) = after.find('"') {
                let rest = &after[q1 + 1..];
                if let Some(q2) = rest.find('"') {
                    let title = rest[..q2].trim();
                    if !title.is_empty() {
                        return Some(title.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Parse the `X:` / `T:` headers of an ABC file into a per-tune index. The first `T:`
/// after an `X:` is taken as the tune title.
fn parse_abc_tunes(content: &str) -> Vec<Tune> {
    let mut tunes: Vec<Tune> = Vec::new();
    let mut current: Option<Tune> = None;
    for line in content.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("X:") {
            if let Some(t) = current.take() {
                tunes.push(t);
            }
            let number = rest.trim().parse().unwrap_or(tunes.len() as u32 + 1);
            current = Some(Tune {
                number,
                title: String::new(),
            });
        } else if let Some(rest) = line.strip_prefix("T:") {
            if let Some(t) = current.as_mut() {
                if t.title.is_empty() {
                    t.title = rest.trim().to_string();
                }
            }
        }
    }
    if let Some(t) = current.take() {
        tunes.push(t);
    }
    for t in &mut tunes {
        if t.title.is_empty() {
            t.title = format!("Tune {}", t.number);
        }
    }
    tunes
}

/// A node in the browsed directory tree. Built only from supported files, so directories
/// that contain no `.ly`/`.abc` (anywhere in their subtree) never appear.
#[derive(Default)]
pub struct DirNode {
    pub name: String,
    pub dirs: Vec<DirNode>,
    /// Indices into the scanned [`FileEntry`] list for files directly in this directory.
    pub files: Vec<usize>,
}

/// Build a directory tree relative to `root` from the scanned entries.
pub fn build_tree(root: &Path, entries: &[FileEntry]) -> DirNode {
    let mut tree = DirNode {
        name: root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        ..Default::default()
    };
    for (i, e) in entries.iter().enumerate() {
        let rel = e.path.strip_prefix(root).unwrap_or(e.path.as_path());
        let comps: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        insert(&mut tree, &comps, i);
    }
    sort_tree(&mut tree);
    tree
}

fn insert(node: &mut DirNode, comps: &[String], file_idx: usize) {
    if comps.len() <= 1 {
        node.files.push(file_idx);
        return;
    }
    let dir = &comps[0];
    let pos = match node.dirs.iter().position(|d| &d.name == dir) {
        Some(p) => p,
        None => {
            node.dirs.push(DirNode {
                name: dir.clone(),
                ..Default::default()
            });
            node.dirs.len() - 1
        }
    };
    insert(&mut node.dirs[pos], &comps[1..], file_idx);
}

fn sort_tree(node: &mut DirNode) {
    node.dirs.sort_by_key(|d| d.name.to_lowercase());
    for d in &mut node.dirs {
        sort_tree(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abc_multi_tune_index() {
        let abc = "X:1\nT:First\nK:C\nCDEF|\n\nX:2\nT:Second\nK:G\nGABc|\n";
        let tunes = parse_abc_tunes(abc);
        assert_eq!(tunes.len(), 2);
        assert_eq!(tunes[0].number, 1);
        assert_eq!(tunes[0].title, "First");
        assert_eq!(tunes[1].title, "Second");
    }

    #[test]
    fn abc_untitled_tune_gets_placeholder() {
        let tunes = parse_abc_tunes("X:7\nK:C\nCDEF|\n");
        assert_eq!(tunes.len(), 1);
        assert_eq!(tunes[0].title, "Tune 7");
    }
}
