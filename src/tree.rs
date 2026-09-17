use crate::diff::DiffFile;

/// A visible entry in the fully expanded changed-files tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub depth: usize,
    pub file: Option<usize>,
}
impl Entry {
    pub fn label(&self) -> String {
        format!(
            "{}{}{}",
            "  ".repeat(self.depth),
            self.path.rsplit('/').next().unwrap_or_default(),
            if self.file.is_none() { "/" } else { "" }
        )
    }
}

pub fn entries(files: &[DiffFile]) -> Vec<Entry> {
    let mut paths = files
        .iter()
        .enumerate()
        .map(|(index, file)| (index, file.path.split('/').collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    // Compare components so a directory's descendants stay together even when
    // similarly named siblings (e.g. src.ts and src/) are present.
    paths.sort_by(|(_, a), (_, b)| a.cmp(b));
    let mut previous = Vec::<&str>::new();
    let mut entries = Vec::new();
    for (index, parts) in paths {
        let Some((_, parents)) = parts.split_last() else {
            continue;
        };
        let common = parents
            .iter()
            .zip(&previous)
            .take_while(|(a, b)| a == b)
            .count();
        let mut path = String::new();
        for (depth, part) in parents.iter().enumerate() {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            if depth >= common {
                entries.push(Entry {
                    path: path.clone(),
                    depth,
                    file: None,
                });
            }
        }
        entries.push(Entry {
            path: parts.join("/"),
            depth: parents.len(),
            file: Some(index),
        });
        previous = parents.to_vec();
    }
    entries
}
