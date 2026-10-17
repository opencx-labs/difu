//! Owned navigation facts extracted with lexical binding resolution. No project code runs.
use anyhow::{Result, bail, ensure};
use oxc_allocator::Allocator;
use oxc_ast::{AstKind, ast::*};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug)]
pub(super) enum Binding {
    Function(Span),
    Local(u32),
    Import { source: String, name: String },
    Namespace(String),
    Unsupported(String),
}
#[derive(Clone, Debug)]
pub(super) struct Reference {
    pub span: Span,
    pub binding: Binding,
}
pub(super) struct File {
    pub source: Arc<str>,
    pub bindings: HashMap<u32, Binding>,
    pub references: Vec<Reference>,
    pub exports: HashMap<String, Binding>,
    pub stars: Vec<String>,
}

pub(super) fn parse(path: &str, source: Arc<str>) -> Result<File> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, SourceType::from_path(path)?).parse();
    ensure!(
        parsed.errors.is_empty() && !parsed.panicked,
        "Cannot reliably parse {path}; definition resolution is unavailable for this syntax"
    );
    let built = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&parsed.program);
    ensure!(
        built.errors.is_empty(),
        "Cannot establish unambiguous bindings in {path}"
    );
    let semantic = built.semantic;
    let scope = semantic.scoping();
    let nodes = semantic.nodes();
    let reference = |id: &IdentifierReference<'_>| {
        id.reference_id
            .get()
            .and_then(|r| scope.get_reference(r).symbol_id())
            .map(|s| Binding::Local(scope.symbol_span(s).start))
            .unwrap_or_else(|| {
                Binding::Unsupported(format!("{} has no local or imported binding", id.name))
            })
    };
    let local = |name: &str| {
        scope
            .get_binding(scope.root_scope_id(), name)
            .map(|s| Binding::Local(scope.symbol_span(s).start))
            .unwrap_or_else(|| {
                Binding::Unsupported(format!("Cannot resolve exported binding {name}"))
            })
    };
    let mut file = File {
        source: source.clone(),
        bindings: HashMap::new(),
        references: Vec::new(),
        exports: HashMap::new(),
        stars: Vec::new(),
    };
    for symbol in scope.symbol_ids() {
        let node = semantic.symbol_declaration(symbol);
        let location = scope.symbol_span(symbol);
        let imported_from = || {
            nodes.ancestor_kinds(node.id()).find_map(|kind| match kind {
                AstKind::ImportDeclaration(import) => Some(import),
                _ => None,
            })
        };
        let binding = if !scope.symbol_redeclarations(symbol).is_empty() {
            Binding::Unsupported("This binding has multiple declarations or overloads".into())
        } else if scope.symbol_is_mutated(symbol) {
            Binding::Unsupported(
                "This binding is reassigned; its runtime function cannot be established".into(),
            )
        } else {
            match node.kind() {
                AstKind::Function(f) if f.body.is_some() => Binding::Function(f.span),
                AstKind::VariableDeclarator(v) if matches!(v.id.kind, BindingPatternKind::BindingIdentifier(_)) => {
                    match v.init.as_ref().map(Expression::get_inner_expression) {
                        Some(Expression::ArrowFunctionExpression(_)) => Binding::Function(v.span),
                        Some(Expression::FunctionExpression(f)) if f.body.is_some() => Binding::Function(v.span),
                        Some(Expression::Identifier(id)) if v.kind == VariableDeclarationKind::Const => reference(id),
                        _ => Binding::Unsupported("This binding is not a directly defined function; resolving its value would require type or runtime analysis".into()),
                    }
                }
                AstKind::ImportSpecifier(s) => match imported_from() {
                    Some(i) if !i.import_kind.is_type() && !s.import_kind.is_type() => Binding::Import { source: i.source.value.to_string(), name: s.imported.name().to_string() },
                    _ => Binding::Unsupported("This is a type-only import, not a callable function".into()),
                },
                AstKind::ImportDefaultSpecifier(_) => match imported_from() {
                    Some(i) if !i.import_kind.is_type() => Binding::Import { source: i.source.value.to_string(), name: "default".into() },
                    _ => Binding::Unsupported("This is a type-only import".into()),
                },
                AstKind::ImportNamespaceSpecifier(_) => match imported_from() {
                    Some(i) if !i.import_kind.is_type() => Binding::Namespace(i.source.value.to_string()),
                    _ => Binding::Unsupported("This is a type-only namespace".into()),
                },
                _ => Binding::Unsupported("This name is a parameter, destructured value, or another binding requiring type analysis; it will not be matched to an unrelated function".into()),
            }
        };
        file.bindings.insert(location.start, binding);
        file.references.push(Reference {
            span: location,
            binding: Binding::Local(location.start),
        });
    }
    for node in nodes.iter() {
        match node.kind() {
            AstKind::IdentifierReference(id) => file.references.push(Reference {
                span: id.span,
                binding: reference(id),
            }),
            AstKind::StaticMemberExpression(member) => {
                let binding = match member.object.get_inner_expression() {
                    Expression::Identifier(id) => match reference(id) {
                        Binding::Local(key) => match file.bindings.get(&key) {
                            Some(Binding::Namespace(source)) => Binding::Import { source: source.clone(), name: member.property.name.to_string() },
                            _ => Binding::Unsupported("This is an object method, not a namespace import. Determining its definition requires type analysis".into()),
                        },
                        other => other,
                    },
                    _ => Binding::Unsupported("The method receiver is computed; its definition cannot be established statically".into()),
                };
                file.references.push(Reference {
                    span: member.property.span,
                    binding,
                });
            }
            _ => {}
        }
    }
    for statement in &parsed.program.body {
        match statement {
            Statement::ExportNamedDeclaration(export) => {
                for s in &export.specifiers {
                    let binding = if export.export_kind.is_type() || s.export_kind.is_type() {
                        Binding::Unsupported("This is a type-only export".into())
                    } else if let Some(source) = &export.source {
                        Binding::Import {
                            source: source.value.to_string(),
                            name: s.local.name().to_string(),
                        }
                    } else {
                        local(&s.local.name())
                    };
                    file.exports.insert(s.exported.name().to_string(), binding);
                }
                if let Some(declaration) = &export.declaration {
                    let range = declaration.span();
                    for symbol in scope.iter_bindings_in(scope.root_scope_id()) {
                        let span = scope.symbol_span(symbol);
                        if range.start <= span.start && span.end <= range.end {
                            file.exports.insert(
                                scope.symbol_name(symbol).to_string(),
                                Binding::Local(span.start),
                            );
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                let binding = match &export.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) if f.body.is_some() => {
                        if let Some(id) = &f.id {
                            local(&id.name)
                        } else {
                            Binding::Function(f.span)
                        }
                    }
                    ExportDefaultDeclarationKind::ArrowFunctionExpression(f) => {
                        Binding::Function(f.span)
                    }
                    ExportDefaultDeclarationKind::FunctionExpression(f) if f.body.is_some() => {
                        Binding::Function(f.span)
                    }
                    ExportDefaultDeclarationKind::Identifier(id) => reference(id),
                    _ => Binding::Unsupported(
                        "The default export is not a directly defined function".into(),
                    ),
                };
                file.exports.insert("default".into(), binding);
            }
            Statement::ExportAllDeclaration(export) if !export.export_kind.is_type() => {
                if let Some(name) = &export.exported {
                    file.exports.insert(
                        name.name().to_string(),
                        Binding::Namespace(export.source.value.to_string()),
                    );
                } else {
                    file.stars.push(export.source.value.to_string());
                }
            }
            _ => {}
        }
    }
    // Direct eval can alter lexical bindings in non-strict JavaScript. Never claim certainty.
    if nodes
        .iter()
        .any(|n| matches!(n.kind(), AstKind::WithStatement(_)))
    {
        bail!("This file uses a with statement; lexical definition resolution is unsafe");
    }
    if nodes.iter().any(|n| matches!(n.kind(), AstKind::CallExpression(c) if matches!(c.callee.get_inner_expression(), Expression::Identifier(i) if i.name == "eval"))) {
        bail!("This file calls eval; runtime binding changes cannot be resolved reliably");
    }
    Ok(file)
}
