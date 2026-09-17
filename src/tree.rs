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

/// Include every matching path and the ancestors needed to keep its hierarchy.
/// Matching a directory also matches the full paths of all its descendants.
pub fn filtered(files: &[DiffFile], query: &str) -> Vec<Entry> {
    let entries = entries(files);
    if query.is_empty() {
        return entries;
    }
    let mut visible = std::collections::HashSet::new();
    for entry in &entries {
        if crate::filter::matches(query, &entry.path) {
            visible.insert(entry.path.as_str());
            let mut path = entry.path.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                visible.insert(parent);
                path = parent;
            }
        }
    }
    entries
        .iter()
        .filter(|entry| visible.contains(entry.path.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtering_retains_ancestors_and_directory_children_without_sibling_prefixes() {
        let files = [
            "src/nested/one.ts",
            "src/nested/two.ts",
            "src-other/three.ts",
            "界/four.ts",
        ]
        .into_iter()
        .map(|path| DiffFile {
            path: path.into(),
            old_path: path.into(),
            status: "modified".into(),
            hunks: vec![],
            additions: 0,
            deletions: 0,
        })
        .collect::<Vec<_>>();
        let paths = |query| {
            filtered(&files, query)
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths("ONE.TS"), ["src", "src/nested", "src/nested/one.ts"]);
        assert_eq!(
            paths("NESTED"),
            [
                "src",
                "src/nested",
                "src/nested/one.ts",
                "src/nested/two.ts"
            ]
        );
        assert_eq!(paths("界"), ["界", "界/four.ts"]);
        assert!(paths("absent").is_empty());
        assert_eq!(filtered(&files, ""), entries(&files));
    }
}
