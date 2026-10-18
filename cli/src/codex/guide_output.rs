//! The model must assign every input hunk before its output can match the schema.
//! Cached guides and the renderer keep the existing chapter-oriented format.
use super::{Chapter, ChapterCategory, Guide};
use crate::diff::Snapshot;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    chapters: Vec<ChapterText>,
    hunk_assignments: BTreeMap<String, Vec<Placement>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChapterText {
    category: ChapterCategory,
    title: String,
    explanation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Placement {
    chapter: usize,
    order: usize,
}

pub(super) fn schema(snapshot: &Snapshot) -> Value {
    let required: Vec<_> = snapshot.units().map(|(_, hunk)| hunk.id.clone()).collect();
    let properties: serde_json::Map<String, Value> = required
        .iter()
        .map(|id| (id.clone(), json!({"$ref":"#/$defs/placements"})))
        .collect();
    json!({
        "type":"object", "additionalProperties":false,
        "required":["chapters","hunk_assignments"],
        "properties":{
            "chapters":{
                "type":"array", "minItems":usize::from(!required.is_empty()),
                "items":{
                    "type":"object", "additionalProperties":false,
                    "required":["category","title","explanation"],
                    "properties":{
                        "category":{"type":"string","enum":["schema","migrations","regular","generated","tests"]},
                        "title":{"type":"string"},
                        "explanation":{"type":"string"}
                    }
                }
            },
            "hunk_assignments":{
                "type":"object", "additionalProperties":false,
                "required":required, "properties":properties
            }
        },
        "$defs":{
            "placements":{
                "type":"array", "minItems":1,
                "items":{
                    "type":"object", "additionalProperties":false,
                    "required":["chapter","order"],
                    "properties":{
                        "chapter":{"type":"integer","minimum":0},
                        "order":{"type":"integer","minimum":0}
                    }
                }
            }
        }
    })
}

pub(super) fn parse(bytes: &[u8], snapshot: &Snapshot) -> Result<Guide> {
    let Output {
        chapters,
        mut hunk_assignments,
    } = serde_json::from_slice(bytes)
        .context("Codex returned a guide with an invalid structure")?;
    let mut ordered: Vec<Vec<(usize, String)>> = vec![Vec::new(); chapters.len()];
    for (file, hunk) in snapshot.units() {
        let placements = hunk_assignments.remove(&hunk.id).with_context(|| {
            format!(
                "Guide omitted an assignment for {} ({})",
                hunk.id, file.path
            )
        })?;
        ensure!(
            !placements.is_empty(),
            "Guide has no chapter assignment for {} ({})",
            hunk.id,
            file.path
        );
        let mut seen = HashSet::new();
        for placement in placements {
            let target = ordered.get_mut(placement.chapter).with_context(|| {
                format!(
                    "Guide assigns {} to unknown chapter {}",
                    hunk.id, placement.chapter
                )
            })?;
            if seen.insert(placement.chapter) {
                target.push((placement.order, hunk.id.clone()));
            }
        }
    }
    ensure!(
        hunk_assignments.is_empty(),
        "Guide assigns unknown hunks: {}",
        hunk_assignments
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut chapters: Vec<_> = chapters
        .into_iter()
        .zip(ordered)
        .map(|(text, mut hunks)| {
            // Model-specified order preserves the explanatory sequence within chapters.
            hunks.sort_by_key(|(order, _)| *order);
            Chapter {
                category: text.category,
                title: text.title,
                explanation: text.explanation,
                hunks: hunks.into_iter().map(|(_, id)| id).collect(),
            }
        })
        .collect();
    // Resolve indices before sorting, so section grouping cannot redirect links.
    chapters.sort_by_key(|chapter| chapter.category);
    let guide = Guide { chapters };
    guide.validate(snapshot)?;
    Ok(guide)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Result<Snapshot> {
        let mut files = crate::diff::parse(
            "M\0page.tsx\0M\0image.png\0",
            "diff --git a/page.tsx b/page.tsx\n@@ -1 +1 @@\n-old import\n+new import\n@@ -21 +21 @@\n-old link\n+conditional link\ndiff --git a/image.png b/image.png\nBinary files a/image.png and b/image.png differ\n",
        )?;
        let file = files.first_mut().context("Missing file")?;
        for (index, hunk) in file.hunks.iter_mut().enumerate() {
            hunk.id = format!("f27-h{index}");
        }
        Ok(Snapshot {
            base: "base".into(),
            head: "head".into(),
            merge_base: "merge".into(),
            base_tree: "base-tree".into(),
            head_tree: "head-tree".into(),
            files,
        })
    }

    fn output() -> Value {
        json!({
            "chapters":[
                {"category":"tests","title":"Exercise navigation","explanation":"Tests verify access."},
                {"category":"regular","title":"Gate navigation","explanation":"Access controls the link."}
            ],
            "hunk_assignments":{
                "f27-h0":[{"chapter":1,"order":1}],
                "f27-h1":[{"chapter":0,"order":0},{"chapter":1,"order":0},{"chapter":1,"order":0}],
                "f1-meta":[{"chapter":0,"order":1}]
            }
        })
    }

    #[test]
    fn schema_requires_second_hunks_and_metadata_with_nonempty_placements() -> Result<()> {
        let snapshot = snapshot()?;
        let schema = schema(&snapshot);
        let expected: HashSet<_> = snapshot.units().map(|(_, h)| h.id.as_str()).collect();
        let required: HashSet<_> = schema
            .pointer("/properties/hunk_assignments/required")
            .and_then(Value::as_array)
            .context("required")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let properties: HashSet<_> = schema
            .pointer("/properties/hunk_assignments/properties")
            .and_then(Value::as_object)
            .context("properties")?
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(required, expected);
        assert_eq!(properties, expected);
        assert_eq!(
            schema.pointer("/$defs/placements/minItems"),
            Some(&json!(1))
        );
        assert_eq!(
            schema.pointer("/properties/hunk_assignments/additionalProperties"),
            Some(&json!(false))
        );
        // Emit chapter text before referring to its indices, even without preserve_order.
        let keys: Vec<_> = schema
            .get("properties")
            .and_then(Value::as_object)
            .context("root properties")?
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["chapters", "hunk_assignments"]);
        Ok(())
    }

    #[test]
    fn assignments_preserve_order_reuse_and_category_grouping() -> Result<()> {
        let snapshot = snapshot()?;
        let guide = parse(&serde_json::to_vec(&output())?, &snapshot)?;
        assert_eq!(
            guide
                .chapters
                .iter()
                .map(|c| (c.title.as_str(), c.hunks.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "Gate navigation",
                    vec!["f27-h1".to_owned(), "f27-h0".to_owned()]
                ),
                (
                    "Exercise navigation",
                    vec!["f27-h1".to_owned(), "f1-meta".to_owned()]
                ),
            ]
        );
        let cached: Guide = serde_json::from_slice(&serde_json::to_vec(&guide)?)?;
        cached.validate(&snapshot)?;
        assert_eq!(serde_json::to_value(cached)?, serde_json::to_value(guide)?);
        Ok(())
    }

    #[test]
    fn rejects_missing_empty_unknown_and_out_of_range_assignments() -> Result<()> {
        let snapshot = snapshot()?;
        for replacement in [
            json!([]),
            json!([{"chapter":9,"order":0}]),
            json!([{"chapter":-1,"order":0}]),
        ] {
            let mut value = output();
            *value
                .pointer_mut("/hunk_assignments/f27-h1")
                .context("hunk")? = replacement;
            assert!(parse(&serde_json::to_vec(&value)?, &snapshot).is_err());
        }
        for missing in ["f27-h1", "f1-meta"] {
            let mut value = output();
            value
                .get_mut("hunk_assignments")
                .and_then(Value::as_object_mut)
                .context("assignments")?
                .remove(missing);
            let error = parse(&serde_json::to_vec(&value)?, &snapshot)
                .err()
                .context("expected error")?;
            assert!(error.to_string().contains(missing));
        }
        let mut value = output();
        value
            .get_mut("hunk_assignments")
            .and_then(Value::as_object_mut)
            .context("assignments")?
            .insert("invented".into(), json!([{"chapter":0,"order":0}]));
        assert!(parse(&serde_json::to_vec(&value)?, &snapshot).is_err());
        let mut value = output();
        value
            .get_mut("chapters")
            .and_then(Value::as_array_mut)
            .context("chapters")?
            .push(json!({"category":"regular","title":"Empty","explanation":"No assignments"}));
        assert!(parse(&serde_json::to_vec(&value)?, &snapshot).is_err());
        Ok(())
    }
}
