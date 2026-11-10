//! Deploy-time compilation of JavaScript and TypeScript functions
//! (ADR-0027 §3).
//!
//! The source is parsed with oxc (ES2023 module, TypeScript syntax for
//! `typescript`). TypeScript is type-stripped by oxc's transformer (types,
//! interfaces, `enum`s, namespaces and parameter properties become
//! JavaScript) and printed with oxc's code generator; JavaScript is kept as
//! written. A function is one file: imports other than `"forgebase"` are
//! compile errors, as is a dynamic `import()`.
//!
//! The same pass collects the services the code visibly uses (`ctx.db.from(..)`,
//! `ctx.kv.get(..)`, `ctx.admin.*`, `fetch(..)`, ...) so a deploy can refuse
//! code that needs a capability the version is not granted. The scan is
//! best effort (code that reaches a service through destructuring or computed
//! names is not seen); the check at call time is authoritative.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    CallExpression, ExportAllDeclaration, ExportFromDeclaration, Expression, ImportDeclaration, ImportExpression,
    StaticMemberExpression, TSImportEqualsDeclaration,
};
use oxc_ast_visit::{Visit, walk};
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_transformer::{TransformOptions, Transformer};
use serde::Serialize;
use utoipa::ToSchema;

use crate::model::Language;

/// Largest function source.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// One compile error with its position in the source (1-based line and column, in characters).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CompileDiagnostic {
    /// Line (1-based).
    pub line: u32,
    /// Column (1-based, in characters).
    pub column: u32,
    /// What is wrong.
    pub message: String,
}

/// A capability the code needs: any one of `any_of` satisfies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    /// Capabilities that satisfy the use.
    pub any_of: &'static [&'static str],
    /// What the code uses (`ctx.db`, `fetch`, ...).
    pub what: &'static str,
    /// Where (1-based line and column).
    pub line: u32,
    /// Column.
    pub column: u32,
}

/// Compiled code.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// JavaScript the engine runs.
    pub js: String,
    /// Capabilities the code visibly needs, in source order.
    pub needs: Vec<Need>,
    /// For TypeScript: generated position -> source position (see [`SourceMap`]).
    pub map: Option<SourceMap>,
}

/// A compact source map of the generated JavaScript: sorted
/// `[generated line, generated column, source line, source column]`
/// entries, all 0-based. Stored with the version as JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceMap(pub Vec<[u32; 4]>);

impl SourceMap {
    /// The source position (1-based line and column) of a generated one (1-based).
    pub fn lookup(&self, line: u32, column: u32) -> Option<(u32, u32)> {
        let key = [line.checked_sub(1)?, column.saturating_sub(1)];
        let i = self.0.partition_point(|e| [e[0], e[1]] <= key);
        let e = self.0.get(i.checked_sub(1)?)?;
        (e[0] == key[0]).then(|| (e[2] + 1, e[3] + 1))
    }
}

/// Rewrites `index.js:<line>:<column>` positions in a trace to the user's
/// source (`index.ts` through `map` for TypeScript).
pub fn remap_trace(text: &str, map: Option<&SourceMap>, file: &str) -> String {
    const NEEDLE: &str = "index.js:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(NEEDLE) {
        out.push_str(&rest[..i]);
        let after = &rest[i + NEEDLE.len()..];
        let num = |s: &str| {
            let n: String = s.chars().take_while(char::is_ascii_digit).collect();
            n.parse::<u32>().ok().map(|v| (v, n.len()))
        };
        let parsed = num(after).and_then(|(l, ll)| {
            let tail = after[ll..].strip_prefix(':')?;
            num(tail).map(|(c, cl)| (l, c, ll + 1 + cl))
        });
        match parsed {
            Some((l, c, used)) => {
                let (l, c) = match map {
                    Some(m) => m.lookup(l, c).unwrap_or((l, c)),
                    None => (l, c),
                };
                out.push_str(&format!("{file}:{l}:{c}"));
                rest = &after[used..];
            }
            None => {
                out.push_str(NEEDLE);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Line and column (1-based, characters) of a byte offset.
pub fn position(source: &str, offset: u32) -> (u32, u32) {
    let offset = (offset as usize).min(source.len());
    let mut end = offset;
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let before = &source[..end];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(column).unwrap_or(u32::MAX),
    )
}

fn diag(source: &str, span: Span, message: impl Into<String>) -> CompileDiagnostic {
    let (line, column) = position(source, span.start);
    CompileDiagnostic {
        line,
        column,
        message: message.into(),
    }
}

fn from_oxc(source: &str, d: &oxc_diagnostics::OxcDiagnostic) -> CompileDiagnostic {
    let offset = d
        .labels
        .iter()
        .find(|l| l.primary())
        .or_else(|| d.labels.first())
        .map_or(0, oxc_span::LabeledSpan::offset);
    let (line, column) = position(source, offset);
    let mut message = d.message.to_string();
    if let Some(help) = &d.help {
        message.push_str(&format!(" ({help})"));
    }
    CompileDiagnostic { line, column, message }
}

/// Compiles `source`; on failure returns every error found (at most 20).
pub fn compile(source: &str, language: Language) -> Result<Compiled, Vec<CompileDiagnostic>> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(vec![CompileDiagnostic {
            line: 1,
            column: 1,
            message: format!("the source is larger than {MAX_SOURCE_BYTES} bytes"),
        }]);
    }
    let (source_type, path) = match language {
        Language::Typescript => (SourceType::ts().with_module(true), "index.ts"),
        _ => (SourceType::mjs(), "index.js"),
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    let mut errors: Vec<CompileDiagnostic> = parsed.diagnostics.errors().map(|d| from_oxc(source, d)).collect();
    if !errors.is_empty() {
        errors.truncate(20);
        return Err(errors);
    }
    let mut program = parsed.program;
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .with_enum_eval(true)
        .build(&program);
    errors.extend(semantic.diagnostics.errors().map(|d| from_oxc(source, d)));
    let mut scan = Scan {
        source,
        needs: Vec::new(),
        errors: Vec::new(),
    };
    scan.visit_program(&program);
    errors.extend(scan.errors);
    if !errors.is_empty() {
        errors.sort_by_key(|e| (e.line, e.column));
        errors.truncate(20);
        return Err(errors);
    }
    let needs = scan.needs;
    let mut map = None;
    let js = if matches!(language, Language::Typescript) {
        let scoping = semantic.semantic.into_scoping();
        let options = TransformOptions::default();
        let ret = Transformer::new(&allocator, Path::new(path), &options).build_with_scoping(scoping, &mut program);
        let mut errors: Vec<CompileDiagnostic> = ret.diagnostics.errors().map(|d| from_oxc(source, d)).collect();
        if !errors.is_empty() {
            errors.truncate(20);
            return Err(errors);
        }
        let ret = Codegen::new()
            .with_options(oxc_codegen::CodegenOptions {
                source_map_path: Some(std::path::PathBuf::from(path)),
                ..oxc_codegen::CodegenOptions::default()
            })
            .build(&program);
        if let Some(m) = ret.map {
            let mut entries: Vec<[u32; 4]> = m
                .get_tokens()
                .map(|t| [t.get_dst_line(), t.get_dst_col(), t.get_src_line(), t.get_src_col()])
                .collect();
            entries.sort_unstable();
            entries.dedup_by(|a, b| a[0] == b[0] && a[1] == b[1]);
            map = Some(SourceMap(entries));
        }
        ret.code
    } else {
        source.to_owned()
    };
    Ok(Compiled { js, needs, map })
}

/// Services reachable through `ctx` and the capabilities that satisfy them.
const SERVICES: &[(&str, &str, &[&str])] = &[
    ("db", "ctx.db", &["db.read", "db.write"]),
    ("vector", "ctx.vector", &["vector.read", "vector.write"]),
    ("kv", "ctx.kv", &["kv"]),
    ("storage", "ctx.storage", &["storage.read", "storage.write"]),
    ("queues", "ctx.queues", &["queues.publish"]),
    ("jobs", "ctx.jobs", &["jobs.enqueue"]),
    ("realtime", "ctx.realtime", &["realtime.publish"]),
];

/// `(service, method) -> (what, capabilities)` for calls on `.from(..)` handles.
fn handle_method(service: &str, method: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match (service, method) {
        ("db", "get" | "list") => ("ctx.db reads", &["db.read"]),
        ("db", "create" | "update" | "delete") => ("ctx.db writes", &["db.write"]),
        ("vector", "search" | "get") => ("ctx.vector reads", &["vector.read"]),
        ("vector", "upsert" | "delete") => ("ctx.vector writes", &["vector.write"]),
        ("storage", "get" | "list" | "signedUrl") => ("ctx.storage reads", &["storage.read"]),
        ("storage", "put" | "delete") => ("ctx.storage writes", &["storage.write"]),
        _ => return None,
    })
}

const KV_METHODS: &[&str] = &[
    "get",
    "set",
    "del",
    "incr",
    "decr",
    "expire",
    "ttl",
    "exists",
    "keys",
    "hget",
    "hset",
    "hdel",
    "hgetall",
    "command",
    "namespace",
];

struct Scan<'s> {
    source: &'s str,
    needs: Vec<Need>,
    errors: Vec<CompileDiagnostic>,
}

impl Scan<'_> {
    fn need(&mut self, span: Span, what: &'static str, any_of: &'static [&'static str]) {
        let (line, column) = position(self.source, span.start);
        self.needs.push(Need {
            any_of,
            what,
            line,
            column,
        });
    }

    fn check_specifier(&mut self, span: Span, spec: &str) {
        if spec != "forgebase" {
            self.errors.push(diag(
                self.source,
                span,
                format!(
                    "cannot import {spec:?}: a function is a single file and may import only \"forgebase\" \
                     (relative, npm: and URL imports are not supported)"
                ),
            ));
        }
    }
}

/// `x.<service>` with `<service>` one of [`SERVICES`]: its name.
fn service_of(e: &Expression<'_>) -> Option<&'static str> {
    let Expression::StaticMemberExpression(m) = e else {
        return None;
    };
    SERVICES
        .iter()
        .find(|(name, ..)| m.property.name.as_str() == *name)
        .map(|(name, ..)| *name)
}

impl<'a> Visit<'a> for Scan<'_> {
    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        self.check_specifier(it.source.span, it.source.value.as_str().unwrap_or("?"));
        walk::walk_import_declaration(self, it);
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        self.check_specifier(it.source.span, it.source.value.as_str().unwrap_or("?"));
        walk::walk_export_from_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.check_specifier(it.source.span, it.source.value.as_str().unwrap_or("?"));
        walk::walk_export_all_declaration(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        self.errors.push(diag(
            self.source,
            it.span,
            "dynamic import() is not supported: a function is a single file",
        ));
        walk::walk_import_expression(self, it);
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        self.errors.push(diag(
            self.source,
            it.span,
            "import = require() is not supported: a function is a single file",
        ));
        walk::walk_ts_import_equals_declaration(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if it.property.name.as_str() == "admin" {
            self.need(it.property.span, "ctx.admin", &["service_role"]);
        }
        walk::walk_static_member_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        match &it.callee {
            Expression::Identifier(id) if id.name.as_str() == "fetch" => self.need(id.span, "fetch", &["fetch"]),
            Expression::StaticMemberExpression(m) => {
                let method = m.property.name.as_str();
                // ctx.<svc>.from(..).<method>(..)
                if let Expression::CallExpression(inner) = &m.object
                    && let Expression::StaticMemberExpression(from) = &inner.callee
                    && from.property.name.as_str() == "from"
                    && let Some(svc) = service_of(&from.object)
                {
                    if let Some((what, caps)) = handle_method(svc, method) {
                        self.need(m.property.span, what, caps);
                    }
                } else if let Some(svc) = service_of(&m.object) {
                    let known = match svc {
                        "kv" => KV_METHODS.contains(&method),
                        "queues" => method == "publish",
                        "jobs" => method == "enqueue",
                        "realtime" => method == "broadcast",
                        _ => method == "from",
                    };
                    if known && let Some((_, what, caps)) = SERVICES.iter().find(|(n, ..)| *n == svc) {
                        self.need(m.span(), what, caps);
                    }
                } else if method == "get"
                    && let Expression::StaticMemberExpression(env) = &m.object
                    && env.property.name.as_str() == "env"
                {
                    self.need(env.property.span, "env.get", &["env", "secrets"]);
                }
            }
            _ => {}
        }
        walk::walk_call_expression(self, it);
    }
}

/// Needs of `compiled` that `has` does not satisfy, as diagnostics.
pub fn unmet(compiled: &Compiled, has: impl Fn(&str) -> bool) -> Vec<CompileDiagnostic> {
    let mut out: Vec<CompileDiagnostic> = Vec::new();
    for n in &compiled.needs {
        if n.any_of.iter().any(|c| has(c)) {
            continue;
        }
        let caps = n
            .any_of
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(" or ");
        let message = format!(
            "the code uses {} but the capability {caps} is not granted: add it under Capabilities and deploy again",
            n.what
        );
        if !out.iter().any(|d| d.message == message) {
            out.push(CompileDiagnostic {
                line: n.line,
                column: n.column,
                message,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(src: &str) -> Vec<(&'static str, Vec<&'static str>)> {
        compile(src, Language::Typescript)
            .unwrap()
            .needs
            .into_iter()
            .map(|n| (n.what, n.any_of.to_vec()))
            .collect()
    }

    #[test]
    fn strips_types_and_keeps_forgebase_import() {
        let src = "import { serve } from \"forgebase\";\ninterface T { a: number }\nenum E { A = 1 }\nconst x: T = { a: E.A };\nserve(async (req: Request, ctx: any): Promise<Response> => Response.json(x as T));\n";
        let c = compile(src, Language::Typescript).unwrap();
        assert!(c.js.contains("from \"forgebase\""), "{}", c.js);
        assert!(!c.js.contains("interface"), "{}", c.js);
        assert!(!c.js.contains(": T"), "{}", c.js);
        assert!(c.js.contains("E.A") || c.js.contains("E[\"A\"]"), "{}", c.js);
    }

    #[test]
    fn errors_have_positions() {
        let e = compile("const a = 1;\nconst b = ;\n", Language::Javascript).unwrap_err();
        assert_eq!((e[0].line, e[0].column), (2, 11), "{e:?}");
        let e = compile("let x: number = 1;", Language::Javascript).unwrap_err();
        assert_eq!(e[0].line, 1);
    }

    #[test]
    fn refuses_other_imports() {
        for src in [
            "import x from \"./other.ts\";",
            "import x from \"npm:left-pad\";",
            "import x from \"https://example.com/x.js\";",
            "export * from \"./a.js\";",
            "const m = await import(\"forgebase\");",
        ] {
            let e = compile(src, Language::Typescript).unwrap_err();
            assert!(e[0].message.contains("single file"), "{src}: {e:?}");
        }
    }

    #[test]
    fn collects_needs() {
        let n = caps(
            "serve(async (req, ctx) => { await ctx.db.from(\"t\").list(); await ctx.db.from(\"t\").create({}); \
             await ctx.kv.get(\"k\"); await ctx.admin.db.from(\"t\").get(\"1\"); await fetch(\"https://x\"); \
             ctx.env.get(\"A\"); await ctx.queues.publish(\"t\", 1); await ctx.jobs.enqueue(\"k\", {}); \
             await ctx.realtime.broadcast(\"c\", \"e\", 1); await ctx.storage.from(\"b\").put(\"k\", \"v\"); \
             await ctx.vector.from(\"v\").search({ vector: [1] }); });",
        );
        let whats: Vec<_> = n.iter().map(|(w, _)| *w).collect();
        for w in [
            "ctx.db reads",
            "ctx.db writes",
            "ctx.kv",
            "ctx.admin",
            "fetch",
            "env.get",
            "ctx.queues",
            "ctx.jobs",
            "ctx.realtime",
            "ctx.storage writes",
            "ctx.vector reads",
        ] {
            assert!(whats.contains(&w), "{w} missing from {whats:?}");
        }
        let c = compile(
            "serve((r, ctx) => ctx.db.from(\"t\").create({}));",
            Language::Javascript,
        )
        .unwrap();
        let missing = unmet(&c, |c| c == "db.read");
        assert_eq!(missing.len(), 1);
        assert!(missing[0].message.contains("db.write"));
        assert!(unmet(&c, |c| c == "db.write").is_empty());
    }

    #[test]
    fn traces_point_at_the_typescript_source() {
        let src = "import { serve } from \"forgebase\";\n\ninterface A {\n  a: number;\n}\n\nserve(() => {\n  throw new Error(\"x\");\n});\n";
        let c = compile(src, Language::Typescript).unwrap();
        let map = c.map.as_ref().unwrap();
        // Find where the generated code has the throw.
        let (gl, line) = c.js.lines().enumerate().find(|(_, l)| l.contains("throw")).unwrap();
        let gc = line.find("throw").unwrap();
        let (sl, sc) = map
            .lookup(u32::try_from(gl + 1).unwrap(), u32::try_from(gc + 1).unwrap())
            .unwrap();
        assert_eq!((sl, sc), (8, 3), "{}", c.js);
        let t = remap_trace(
            &format!("Error: x\n    at <anonymous> (index.js:{}:{})", gl + 1, gc + 1),
            Some(map),
            "index.ts",
        );
        assert_eq!(t, "Error: x\n    at <anonymous> (index.ts:8:3)");
        assert_eq!(
            remap_trace("at f (index.js:3:4) index.js:x", None, "index.js"),
            "at f (index.js:3:4) index.js:x"
        );
    }

    #[test]
    fn enums_compile() {
        let c = compile(
            "enum Color { Red = \"r\", Green = \"g\" }\nexport const c = Color.Red;\n",
            Language::Typescript,
        )
        .unwrap();
        assert!(c.js.contains("Color"), "{}", c.js);
    }

    #[test]
    fn positions_count_characters() {
        assert_eq!(position("ab\ncd", 4), (2, 2));
        assert_eq!(position("é\nx", 3), (2, 1));
        assert_eq!(position("é", 1), (1, 1));
    }
}
