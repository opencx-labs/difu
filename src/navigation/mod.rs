//! Import-aware function navigation using only pinned, tracked Git objects.
mod syntax;
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use syntax::{Binding, File};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub root: PathBuf,
    pub revision: String,
    pub path: String,
    pub line: u64,
    /// UTF-8 byte offset within the unmodified source line.
    pub column: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub path: String,
    pub revision: String,
    pub line: usize,
    pub source: String,
}
pub struct Viewer {
    pub id: u64,
    pub request: Request,
    pub output: Option<Result<Arc<Definition>, String>>,
    pub scroll: usize,
    pub horizontal: usize,
    pub viewport: usize,
    pub cancel: Cancel,
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub fn supported(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|s| s.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs")
    )
}

trait Sources {
    fn read(&mut self, path: &str) -> Result<Option<Arc<str>>>;
    fn contains(&self, path: &str) -> bool;
}
struct GitSources<'a> {
    root: &'a Path,
    cancel: &'a Cancel,
    entries: HashMap<String, (String, usize, bool)>,
    cache: HashMap<String, Arc<str>>,
    bytes: usize,
}
impl<'a> GitSources<'a> {
    fn new(root: &'a Path, revision: &str, cancel: &'a Cancel) -> Result<Self> {
        ensure!(
            matches!(revision.len(), 40 | 64) && revision.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid pinned revision"
        );
        let listing = process::checked(
            repo::git(root).args(["ls-tree", "-r", "-l", "-z", "--full-tree", revision]),
            cancel,
        )?;
        let mut entries = HashMap::new();
        for entry in listing.split('\0').filter(|s| !s.is_empty()) {
            let (metadata, path) = entry.split_once('\t').context("Invalid Git tree entry")?;
            let mut fields = metadata.split_whitespace();
            let mode = fields.next();
            let kind = fields.next();
            let oid = fields.next().context("Missing object id")?;
            let size = fields.next().context("Missing object size")?;
            if kind == Some("blob") {
                entries.insert(
                    path.to_owned(),
                    (
                        oid.to_owned(),
                        size.parse()?,
                        matches!(mode, Some("100644" | "100755")),
                    ),
                );
            }
        }
        Ok(Self {
            root,
            cancel,
            entries,
            cache: HashMap::new(),
            bytes: 0,
        })
    }
}
impl Sources for GitSources<'_> {
    fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }
    fn read(&mut self, path: &str) -> Result<Option<Arc<str>>> {
        self.cancel.check()?;
        if let Some(source) = self.cache.get(path) {
            return Ok(Some(source.clone()));
        }
        let Some((oid, size, regular)) = self.entries.get(path) else {
            return Ok(None);
        };
        ensure!(
            *regular,
            "{path} is a symbolic link; definition navigation only reads regular tracked files"
        );
        ensure!(
            *size <= 4 * 1024 * 1024
                && self.bytes.saturating_add(*size) <= 32 * 1024 * 1024
                && self.cache.len() < 128,
            "Definition resolution reached its source-size limit at {path}"
        );
        let source: Arc<str> = process::checked(
            repo::git(self.root).args(["cat-file", "blob", oid]),
            self.cancel,
        )?
        .into();
        self.bytes = self.bytes.saturating_add(source.len());
        self.cache.insert(path.to_owned(), source.clone());
        Ok(Some(source))
    }
}

pub fn resolve(request: &Request, cancel: &Cancel) -> Result<Definition> {
    let sources = GitSources::new(&request.root, &request.revision, cancel)?;
    let mut resolver = Resolver {
        sources,
        files: HashMap::new(),
        cancel,
        entry: request.path.clone(),
        project: None,
    };
    resolver.at(request)
}
struct Resolver<'a, S> {
    sources: S,
    files: HashMap<String, Arc<File>>,
    cancel: &'a Cancel,
    entry: String,
    project: Option<Config>,
}
#[derive(Default, Clone)]
struct Config {
    base: Option<String>,
    paths: Vec<(String, Vec<String>, String)>,
    paths_set: bool,
    unsupported: Vec<String>,
}
impl<S: Sources> Resolver<'_, S> {
    fn file(&mut self, path: &str) -> Result<Arc<File>> {
        self.cancel.check()?;
        ensure!(
            supported(path),
            "{path} is not a supported JavaScript or TypeScript source file"
        );
        if let Some(file) = self.files.get(path) {
            return Ok(file.clone());
        }
        ensure!(
            self.files.len() < 64,
            "Import resolution exceeded 64 source files"
        );
        let source = self.sources.read(path)?.with_context(|| {
            format!("{path} is not an available tracked source file in this revision")
        })?;
        let file = Arc::new(syntax::parse(path, source)?);
        self.files.insert(path.to_owned(), file.clone());
        Ok(file)
    }
    fn at(&mut self, request: &Request) -> Result<Definition> {
        let file = self.file(&request.path)?;
        let mut offset = 0;
        let mut found = None;
        for (index, line) in file.source.split_inclusive('\n').enumerate() {
            if index as u64 + 1 == request.line {
                ensure!(
                    request.column < line.trim_end_matches(['\r', '\n']).len(),
                    "No source symbol at this position"
                );
                found = Some(offset + request.column);
                break;
            }
            offset += line.len();
        }
        let offset = found.context("This line is not present in the pinned source")?;
        let reference = file
            .references
            .iter()
            .filter(|r| r.span.start as usize <= offset && offset < r.span.end as usize)
            .min_by_key(|r| r.span.size())
            .context("This position is not a resolvable function reference")?;
        let (path, span) = self.binding(
            &request.path,
            reference.binding.clone(),
            &mut HashSet::new(),
            0,
        )?;
        let source = self.file(&path)?;
        let before = source
            .source
            .get(..span.start as usize)
            .context("Invalid definition range")?;
        let body = source
            .source
            .get(span.start as usize..span.end as usize)
            .context("Invalid definition range")?;
        Ok(Definition {
            path,
            revision: request.revision.clone(),
            line: before.bytes().filter(|b| *b == b'\n').count() + 1,
            source: body.to_owned(),
        })
    }
    fn binding(
        &mut self,
        path: &str,
        binding: Binding,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(String, oxc_span::Span)> {
        self.cancel.check()?;
        ensure!(
            depth < 64,
            "Import or alias chain exceeds the navigation limit"
        );
        match binding {
            Binding::Function(span) => Ok((path.to_owned(), span)),
            Binding::Local(key) => {
                let identity = format!("{path}:local:{key}");
                ensure!(
                    visited.insert(identity.clone()),
                    "Cyclic local function alias"
                );
                let file = self.file(path)?;
                let binding = file
                    .bindings
                    .get(&key)
                    .context("Missing lexical binding")?
                    .clone();
                let result = self.binding(path, binding, visited, depth + 1);
                visited.remove(&identity);
                result
            }
            Binding::Import { source, name } => {
                let target = self.module(path, &source)?;
                self.export(&target, &name, visited, depth + 1)?
                    .with_context(|| {
                        format!("{target} does not export a resolvable function named {name}")
                    })
            }
            Binding::Namespace(_) => {
                bail!("This name refers to a module namespace; select a function on that namespace")
            }
            Binding::Unsupported(reason) => bail!("{reason}"),
        }
    }
    fn export(
        &mut self,
        path: &str,
        name: &str,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> Result<Option<(String, oxc_span::Span)>> {
        ensure!(depth < 64, "Re-export chain exceeds the navigation limit");
        let identity = format!("{path}:export:{name}");
        ensure!(
            visited.insert(identity.clone()),
            "Cyclic re-export chain prevents reliable resolution"
        );
        let result = (|| {
            let file = self.file(path)?;
            if let Some(binding) = file.exports.get(name) {
                return self
                    .binding(path, binding.clone(), visited, depth + 1)
                    .map(Some);
            }
            if name == "default" {
                return Ok(None);
            }
            let mut target = None;
            for source in &file.stars {
                let module = self.module(path, source)?;
                if let Some(next) = self.export(&module, name, visited, depth + 1)? {
                    ensure!(
                        target.as_ref().is_none_or(|previous| previous == &next),
                        "Multiple star exports provide {name}; there is no unique definition"
                    );
                    target = Some(next);
                }
            }
            Ok(target)
        })();
        visited.remove(&identity);
        result
    }
    fn config(&mut self, path: &str) -> Result<Config> {
        let mut directory = parent(path).to_owned();
        loop {
            for name in ["tsconfig.json", "jsconfig.json"] {
                let path = join(&directory, name)?;
                if self.sources.contains(&path) {
                    return self.load_config(&path, &mut HashSet::new());
                }
            }
            if directory.is_empty() {
                break;
            }
            directory = parent(&directory).to_owned();
        }
        Ok(Config::default())
    }
    fn load_config(&mut self, path: &str, visited: &mut HashSet<String>) -> Result<Config> {
        ensure!(
            visited.len() < 16 && visited.insert(path.to_owned()),
            "Cyclic or excessively deep project configuration"
        );
        let text = self
            .sources
            .read(path)?
            .with_context(|| format!("Cannot read project configuration {path}"))?;
        let value = jsonc_parser::parse_to_serde_value(&text, &Default::default())?
            .context("Empty project configuration")?;
        let mut config = Config::default();
        if let Some(extends) = value.get("extends") {
            let paths: Vec<&Value> = match extends {
                Value::Array(paths) => paths.iter().collect(),
                other => vec![other],
            };
            for base in paths {
                let base = base.as_str().context("Invalid configuration extends")?;
                ensure!(
                    base.starts_with('.'),
                    "Configuration extends {base}; installed or external configuration packages are not resolved"
                );
                let mut target = join(parent(path), base)?;
                if !self.sources.contains(&target) {
                    target.push_str(".json");
                }
                let inherited = self.load_config(&target, visited)?;
                if inherited.base.is_some() {
                    config.base = inherited.base;
                }
                if inherited.paths_set {
                    config.paths = inherited.paths;
                    config.paths_set = true;
                }
                config.unsupported.extend(inherited.unsupported);
            }
        }
        if let Some(options) = value.get("compilerOptions") {
            if let Some(base) = options.get("baseUrl") {
                config.base = Some(join(
                    parent(path),
                    base.as_str().context("Invalid baseUrl configuration")?,
                )?);
            }
            if let Some(paths) = options.get("paths") {
                let paths = paths.as_object().context("Invalid paths configuration")?;
                config.paths.clear();
                config.paths_set = true;
                for (pattern, targets) in paths {
                    ensure!(
                        pattern.matches('*').count() <= 1,
                        "Invalid path alias pattern {pattern}"
                    );
                    let targets = targets
                        .as_array()
                        .context("Invalid alias targets")?
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .context("Invalid alias target")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    ensure!(
                        targets
                            .iter()
                            .all(|target| target.matches('*').count() <= 1),
                        "Invalid path alias targets for {pattern}"
                    );
                    config
                        .paths
                        .push((pattern.clone(), targets, parent(path).to_owned()));
                }
            }
            for option in ["rootDirs", "moduleSuffixes"] {
                if options.get(option).is_some() {
                    config.unsupported.push(option.to_owned());
                }
            }
        }
        visited.remove(path);
        Ok(config)
    }
    fn module(&mut self, from: &str, specifier: &str) -> Result<String> {
        self.cancel.check()?;
        // Compiler options belong to the originating project, including when
        // following a re-export into a directory with another tsconfig.
        if self.project.is_none() {
            self.project = Some(self.config(&self.entry.clone())?);
        }
        let config = self
            .project
            .as_ref()
            .context("Missing project configuration")?
            .clone();
        ensure!(
            config.unsupported.is_empty(),
            "Project uses {}, which this resolver cannot establish reliably",
            config.unsupported.join(", ")
        );
        if specifier.starts_with("./") || specifier.starts_with("../") {
            return self
                .candidate(&join(parent(from), specifier)?)?
                .with_context(|| format!("Cannot find tracked import {specifier} from {from}"));
        }
        let mut matching = Vec::new();
        for (pattern, targets, base) in &config.paths {
            let capture = if let Some((prefix, suffix)) = pattern.split_once('*') {
                specifier
                    .strip_prefix(prefix)
                    .and_then(|s| s.strip_suffix(suffix))
                    .map(|s| (prefix.len(), s))
            } else if pattern == specifier {
                Some((usize::MAX, ""))
            } else {
                None
            };
            if let Some((priority, capture)) = capture {
                matching.push((
                    priority,
                    targets,
                    config.base.as_deref().unwrap_or(base),
                    capture,
                ));
            }
        }
        matching.sort_by_key(|m| std::cmp::Reverse(m.0));
        if let Some((priority, targets, base, capture)) = matching.first() {
            ensure!(
                matching.iter().filter(|m| m.0 == *priority).count() == 1,
                "Multiple path aliases match {specifier}"
            );
            let mut found = None;
            for target in *targets {
                if let Some(path) = self.candidate(&join(base, &target.replace('*', capture))?)? {
                    ensure!(
                        found.as_ref().is_none_or(|p| p == &path),
                        "Multiple alias targets resolve {specifier}; cannot choose a unique definition"
                    );
                    found = Some(path);
                }
            }
            return found.with_context(|| format!("No tracked source matches alias {specifier}"));
        }
        if let Some(base) = &config.base
            && let Some(path) = self.candidate(&join(base, specifier)?)?
        {
            return Ok(path);
        }
        bail!(
            "Import {specifier} is a package, workspace package, or unmapped alias. Its target cannot be verified from tracked source and paths configuration; dependencies are not downloaded"
        )
    }
    fn candidate(&self, path: &str) -> Result<Option<String>> {
        ensure!(
            !path.split('/').any(|s| s == "node_modules"),
            "Installed dependencies are outside tracked-source navigation"
        );
        let mut candidates = Vec::new();
        let extension = Path::new(path).extension().and_then(|e| e.to_str());
        match extension {
            Some("js" | "jsx" | "mjs" | "cjs") => {
                let stem = path
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .context("Missing source extension")?;
                let extensions: &[&str] = match extension {
                    Some("mjs") => &["mts", "mjs"],
                    Some("cjs") => &["cts", "cjs"],
                    _ => &["ts", "tsx", "js", "jsx"],
                };
                candidates.extend(extensions.iter().map(|ext| format!("{stem}.{ext}")));
            }
            Some(_) => candidates.push(path.to_owned()),
            None => {
                for suffix in [
                    ".ts",
                    ".tsx",
                    ".js",
                    ".jsx",
                    "/index.ts",
                    "/index.tsx",
                    "/index.js",
                    "/index.jsx",
                ] {
                    candidates.push(format!("{path}{suffix}"));
                }
                ensure!(
                    !self.sources.contains(&format!("{path}/package.json")),
                    "Import {path} has package entry-point configuration; directory resolution cannot be verified"
                );
            }
        }
        let existing: Vec<_> = candidates
            .into_iter()
            .filter(|p| self.sources.contains(p))
            .collect();
        ensure!(
            existing.len() <= 1,
            "Multiple tracked source files match import {path}; cannot verify a unique target"
        );
        Ok(existing.into_iter().next())
    }
}
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}
fn join(base: &str, path: &str) -> Result<String> {
    ensure!(
        !path.starts_with('/') && !path.contains('\\') && !path.contains('\0'),
        "Import path is outside repository-relative source"
    );
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                ensure!(parts.pop().is_some(), "Import escapes the repository");
            }
            value => parts.push(value),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Memory(HashMap<String, Arc<str>>);
    impl Sources for Memory {
        fn contains(&self, path: &str) -> bool {
            self.0.contains_key(path)
        }
        fn read(&mut self, path: &str) -> Result<Option<Arc<str>>> {
            Ok(self.0.get(path).cloned())
        }
    }
    fn lookup(files: &[(&str, &str)], path: &str, needle: &str) -> Result<Definition> {
        let source = files
            .iter()
            .find(|(p, _)| *p == path)
            .context("Missing fixture")?
            .1;
        let offset = source.rfind(needle).context("Missing fixture reference")?;
        let prefix = source.get(..offset).context("Fixture prefix")?;
        let request = Request {
            root: PathBuf::new(),
            revision: "a".repeat(40),
            path: path.into(),
            line: prefix.bytes().filter(|b| *b == b'\n').count() as u64 + 1,
            column: prefix.rsplit('\n').next().unwrap_or_default().len(),
        };
        let memory = Memory(
            files
                .iter()
                .map(|(p, s)| (p.to_string(), Arc::from(*s)))
                .collect(),
        );
        Resolver {
            sources: memory,
            files: HashMap::new(),
            cancel: &Cancel::default(),
            entry: path.into(),
            project: None,
        }
        .at(&request)
    }
    #[test]
    fn follows_imports_aliases_reexports_and_namespace_members() -> Result<()> {
        let files = [
            (
                "src/call.ts",
                "import main from './barrel'; import { renamed as run } from './barrel'; import * as api from './barrel';\nmain(); run(); api.renamed();",
            ),
            (
                "src/barrel.ts",
                "export { original as renamed } from './impl'; export { default } from './impl';",
            ),
            (
                "src/impl.ts",
                "export const original = (value: number = 1) => value + 1;\nexport default function main() { return original(); }",
            ),
        ];
        for name in ["run()", "renamed()"] {
            let found = lookup(&files, "src/call.ts", name)?;
            assert_eq!(found.path, "src/impl.ts");
            assert!(found.source.contains("value + 1"));
        }
        let found = lookup(&files, "src/call.ts", "main()")?;
        assert_eq!(found.line, 2);
        assert!(found.source.starts_with("function main"));
        Ok(())
    }
    #[test]
    fn respects_shadowing_and_rejects_runtime_values() -> Result<()> {
        let library = ("impl.ts", "export function run() { return 'imported'; }");
        let local = [
            library,
            (
                "call.ts",
                "import {run} from './impl'; function outer() { function run() { return 'local'; } run(); }",
            ),
        ];
        assert!(
            lookup(&local, "call.ts", "run();")?
                .source
                .contains("'local'")
        );
        for source in [
            "import {run} from './impl'; function outer(run: () => void) { run(); }",
            "import {run} from './impl'; function outer({run}: any) { run(); }",
            "let run = () => 1; run = () => 2; run();",
            "const object = { run() {} }; object.run();",
            "import {run} from './impl'; function outer() { const run = factory(); run(); }",
        ] {
            assert!(
                lookup(&[library, ("call.ts", source)], "call.ts", "run();").is_err(),
                "must not guess for {source}"
            );
        }
        let alias = [(
            "call.ts",
            "const original = () => 42; const alias = original; alias();",
        )];
        assert_eq!(
            lookup(&alias, "call.ts", "alias();")?.source,
            "original = () => 42"
        );
        Ok(())
    }
    #[test]
    fn resolves_jsonc_paths_and_inherited_configuration() -> Result<()> {
        let files = [
            (
                "tsconfig.base.json",
                "{ // base aliases\n \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@lib/*\": [\"lib/*\"],},},}",
            ),
            (
                "src/tsconfig.json",
                "{\"extends\":\"../tsconfig.base.json\"}",
            ),
            (
                "src/call.tsx",
                "import {widget as Widget} from '@lib/widget';\nconst tree = <div/>; Widget();",
            ),
            (
                "lib/widget.tsx",
                "export function widget() { return <div>Hello</div>; }",
            ),
        ];
        assert_eq!(
            lookup(&files, "src/call.tsx", "Widget();")?.path,
            "lib/widget.tsx"
        );
        Ok(())
    }
    #[test]
    fn star_exports_require_a_unique_target_and_cycles_stop() -> Result<()> {
        let files = [
            ("call.js", "import {run} from './barrel'; run();"),
            ("barrel.js", "export * from './one'; export * from './two';"),
            ("one.js", "export function run() { return 1; }"),
            ("two.js", "export function run() { return 2; }"),
        ];
        assert!(lookup(&files, "call.js", "run();").is_err());
        let cycle = [
            ("call.js", "import {run} from './one'; run();"),
            ("one.js", "export {run} from './two';"),
            ("two.js", "export {run} from './one';"),
        ];
        assert!(lookup(&cycle, "call.js", "run();").is_err());
        Ok(())
    }
    #[test]
    fn resolves_local_jsx_functions_and_unique_star_exports() -> Result<()> {
        let files = [
            (
                "call.jsx",
                "import {Widget} from './barrel'; const node = <Widget/>;",
            ),
            (
                "barrel.js",
                "export * from './widget'; export * from './other';",
            ),
            (
                "widget.jsx",
                "export const Widget = () => <div>Hello</div>;",
            ),
            ("other.js", "export const unrelated = 1;"),
        ];
        assert_eq!(lookup(&files, "call.jsx", "Widget/>")?.path, "widget.jsx");
        Ok(())
    }
    #[test]
    fn configuration_overrides_do_not_reuse_stale_alias_bases() -> Result<()> {
        let files = [
            (
                "base.json",
                r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["lib/*"]}}}"#,
            ),
            (
                "src/tsconfig.json",
                r#"{"extends":"../base.json","compilerOptions":{"baseUrl":"."}}"#,
            ),
            ("src/call.ts", "import {run} from '@/impl'; run();"),
            (
                "src/lib/impl.ts",
                "export function run() {return 'correct';}",
            ),
            ("lib/impl.ts", "export function run() {return 'incorrect';}"),
        ];
        assert_eq!(
            lookup(&files, "src/call.ts", "run();")?.path,
            "src/lib/impl.ts"
        );
        Ok(())
    }
    #[test]
    fn unsupported_or_ambiguous_imports_explain_instead_of_guessing() {
        for files in [
            vec![
                ("call.ts", "import {run} from './impl'; run();"),
                ("impl.ts", "export function run() {}"),
                ("impl.js", "export function run() {}"),
            ],
            vec![
                ("call.ts", "import {run} from 'external'; run();"),
                ("external.ts", "export function run() {}"),
            ],
            vec![
                ("call.ts", "import {run} from './impl'; run();"),
                ("impl.ts", "export function run() {}"),
                ("tsconfig.json", r#"{"extends":"external-config"}"#),
            ],
            vec![
                ("call.ts", "import {run} from './impl'; run();"),
                ("impl.ts", "export function run() {}"),
                (
                    "tsconfig.json",
                    r#"{"compilerOptions":{"rootDirs":["src","generated"]}}"#,
                ),
            ],
            vec![
                ("call.ts", "import type {run} from './impl'; run();"),
                ("impl.ts", "export function run() {}"),
            ],
            vec![
                ("call.ts", "import {run} from './impl'; run();"),
                ("impl.ts", "export function run( {}"),
            ],
            vec![("call.js", "function run() {} eval('run = other'); run();")],
        ] {
            let path = files.first().map_or("call.ts", |(path, _)| *path);
            let error = lookup(&files, path, "run();").err();
            assert!(error.is_some_and(|e| !e.to_string().is_empty()));
        }
    }
    #[test]
    fn reexports_keep_the_callers_project_configuration() -> Result<()> {
        let files = [
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@impl":["correct.ts"]}}}"#,
            ),
            ("call.ts", "import {run} from './nested/barrel'; run();"),
            ("nested/barrel.ts", "export {run} from '@impl';"),
            (
                "nested/tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@impl":["wrong.ts"]}}}"#,
            ),
            ("correct.ts", "export function run() {return 'correct';}"),
            ("nested/wrong.ts", "export function run() {return 'wrong';}"),
        ];
        assert_eq!(lookup(&files, "call.ts", "run();")?.path, "correct.ts");
        Ok(())
    }
}
