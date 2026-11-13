//! `forgebase new <dir> [--template basic|functions|realtime-chat]`: an app
//! project with the layout of ADR-0037, a sample collection and function,
//! seed fixtures, a `.env.example` and a GitHub Actions workflow.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::ValueEnum;
use serde_json::{Value, json};

use super::layout;
use super::model::{self, Kind};

/// Where release archives are downloaded from in the generated workflow.
pub const RELEASES_URL: &str = "https://github.com/farzanshibu/forgebase/releases/download";

const README: &str = include_str!("../../templates/project/README.md");
const GITIGNORE: &str = include_str!("../../templates/project/gitignore");
const WORKFLOW: &str = include_str!("../../templates/project/github-workflow.yml");
const HELLO: &str = include_str!("../../templates/project/functions/hello.ts");
const ORDER_CREATED: &str = include_str!("../../templates/project/functions/order-created.ts");
const MODERATE: &str = include_str!("../../templates/project/functions/moderate.ts");
const FORGEBASE_DTS: &str = include_str!("../../../../sdk/functions/forgebase.d.ts");

/// A starting point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Template {
    /// A to-do collection with access rules, a bucket, an HTTP function and seed data.
    Basic,
    /// Orders with a record hook (and a secret), an HTTP function, a topic fed by the collection and a subscription.
    Functions,
    /// Rooms and messages with relations, indexes, rules and a moderation hook.
    RealtimeChat,
}

/// Options of `new`.
#[derive(Debug, Clone)]
pub struct Options {
    /// App directory.
    pub dir: PathBuf,
    /// Template.
    pub template: Template,
    /// Project name (default: the directory name).
    pub name: Option<String>,
    /// Name of the project directory inside the app (default `forgebase`).
    pub project_dir: String,
    /// Write into a non-empty directory.
    pub force: bool,
}

struct Files {
    root: PathBuf,
    written: Vec<PathBuf>,
}

impl Files {
    fn put(&mut self, rel: impl AsRef<Path>, text: &str) -> anyhow::Result<()> {
        let p = self.root.join(rel.as_ref());
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        std::fs::write(&p, text).with_context(|| format!("cannot write {}", p.display()))?;
        self.written.push(rel.as_ref().to_owned());
        Ok(())
    }

    fn resource(&mut self, pdir: &str, kind: Kind, name: &str, body: Value) -> anyhow::Result<()> {
        let rel = Path::new(pdir)
            .join(layout::kind_dir(kind))
            .join(format!("{}.json", layout::encode_stem(name)));
        self.put(rel, &model::pretty(&layout::with_schema(kind, name, body)))
    }

    fn function(&mut self, pdir: &str, name: &str, toml: &str, source: &str) -> anyhow::Result<()> {
        let fdir = Path::new(pdir).join("functions").join(name);
        self.put(
            fdir.join("function.toml"),
            &format!("#:schema ../../{}/function.schema.json\n{toml}", layout::SCHEMAS_DIR),
        )?;
        self.put(fdir.join("index.ts"), source)?;
        self.put(fdir.join("forgebase.d.ts"), FORGEBASE_DTS)
    }

    fn seed(&mut self, pdir: &str, collection: &str, records: &[Value]) -> anyhow::Result<()> {
        let text: String = records
            .iter()
            .map(|r| format!("{}\n", model::canonical_text(r)))
            .collect();
        self.put(Path::new(pdir).join("seed").join(format!("{collection}.jsonl")), &text)
    }
}

const SIGNED_IN: &str = "@request.auth.id != \"\"";

fn rules_all(rule: &str) -> Value {
    json!({"list_rule": rule, "view_rule": rule, "create_rule": rule, "update_rule": rule, "delete_rule": rule})
}

/// Writes the project; returns the files written (relative to the app directory).
pub fn create(o: &Options, out: &mut impl Write) -> anyhow::Result<Vec<PathBuf>> {
    if o.dir.exists() {
        let non_empty = std::fs::read_dir(&o.dir)
            .with_context(|| format!("cannot read {}", o.dir.display()))?
            .next()
            .is_some();
        if non_empty && !o.force {
            bail!(
                "{} exists and is not empty (pass --force to add the project to it)",
                o.dir.display()
            );
        }
    }
    let name = o.name.clone().unwrap_or_else(|| {
        std::path::absolute(&o.dir)
            .ok()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "app".into())
    });
    let pdir = o.project_dir.as_str();
    if pdir.is_empty() || Path::new(pdir).is_absolute() {
        bail!("--project-dir is a relative directory name");
    }
    let mut f = Files {
        root: o.dir.clone(),
        written: Vec::new(),
    };
    f.put(
        Path::new(pdir).join(layout::PROJECT_FILE),
        &layout::project_file(&name, Some("http://127.0.0.1:8090")),
    )?;
    let schemas = o.dir.join(pdir);
    std::fs::create_dir_all(&schemas)?;
    layout::write_schemas(&schemas)?;
    let mut secrets: Vec<&str> = Vec::new();
    match o.template {
        Template::Basic => basic(&mut f, pdir)?,
        Template::Functions => {
            functions(&mut f, pdir)?;
            secrets.push("SLACK_WEBHOOK_URL");
        }
        Template::RealtimeChat => chat(&mut f, pdir)?,
    }
    let mut env = String::from(
        "# Where `forgebase project` and `forgebase deploy` connect, and as whom.\n# Copy to .env (never commit it).\nFORGEBASE_URL=http://127.0.0.1:8090\n# An API key (fbk_...) of a service account with the admin role, or a personal access token (fbp_...).\nFORGEBASE_TOKEN=\n",
    );
    if !secrets.is_empty() {
        env.push_str("\n# Function secrets: read when you apply, never stored in the project.\n");
        for s in &secrets {
            env.push_str(&format!("{s}=\n"));
        }
    }
    f.put(".env.example", &env)?;
    f.put(".gitignore", GITIGNORE)?;
    let secret_list: String = secrets.iter().map(|s| format!(", {s}")).collect();
    let secret_env: String = secrets
        .iter()
        .map(|s| format!("\n          {s}: ${{{{ secrets.{s} }}}}"))
        .collect();
    let readme_secrets = if secrets.is_empty() {
        String::new()
    } else {
        format!(", plus the function secrets {}", secrets.join(", "))
    };
    f.put(
        "README.md",
        &README
            .replace("{{NAME}}", &name)
            .replace("{{DIR}}", pdir)
            .replace("{{SECRETS}}", &readme_secrets),
    )?;
    f.put(
        ".github/workflows/forgebase.yml",
        &WORKFLOW
            .replace("{{DIR}}", pdir)
            .replace("{{VERSION}}", concat!("v", env!("CARGO_PKG_VERSION")))
            .replace("{{RELEASES}}", RELEASES_URL)
            .replace("{{SECRET_LIST}}", &secret_list)
            .replace("{{SECRET_ENV}}", &secret_env),
    )?;
    let files = f.written;
    writeln!(
        out,
        "created {} ({:?} template, {} files)",
        o.dir.display(),
        o.template,
        files.len()
    )?;
    writeln!(
        out,
        "next:\n  cd {}\n  cp .env.example .env   # set FORGEBASE_URL and FORGEBASE_TOKEN\n  forgebase project plan\n  forgebase deploy",
        o.dir.display()
    )?;
    Ok(files)
}

fn basic(f: &mut Files, p: &str) -> anyhow::Result<()> {
    f.resource(
        p,
        Kind::Collection,
        "todos",
        json!({
            "type": "base",
            "fields": [
                {"name": "title", "type": "text", "required": true, "max_length": 200},
                {"name": "done", "type": "bool", "default": false},
                {"name": "due", "type": "date"},
                {"name": "notes", "type": "text", "max_length": 10000}
            ],
            "indexes": [{"name": "todos_done_due", "keys": [{"path": "done"}, {"path": "due"}]}],
            "rules": rules_all(SIGNED_IN)
        }),
    )?;
    f.resource(
        p,
        Kind::Bucket,
        "attachments",
        json!({"visibility": "authenticated", "versioning": false, "max_object_bytes": 10_485_760, "allowed_content_types": ["image/*", "application/pdf"]}),
    )?;
    f.function(
        p,
        "hello",
        "description = \"Says hello over HTTP\"\naccess = \"public\"\nentry = \"index.ts\"\ncapabilities = [\"log\"]\n\n[limits]\ntimeout_ms = 5000\n",
        HELLO,
    )?;
    f.seed(
        p,
        "todos",
        &[
            json!({"title": "Read the ForgeBase docs", "done": true}),
            json!({"title": "Model the app's collections", "done": false}),
            json!({"title": "Deploy from CI", "done": false, "notes": "forgebase deploy --auto-approve"}),
        ],
    )
}

fn functions(f: &mut Files, p: &str) -> anyhow::Result<()> {
    f.resource(
        p,
        Kind::Collection,
        "orders",
        json!({
            "type": "base",
            "fields": [
                {"name": "customer", "type": "email", "required": true},
                {"name": "total", "type": "number", "required": true, "min": 0},
                {"name": "status", "type": "select", "values": ["pending", "paid", "shipped"], "default": "pending"},
                {"name": "items", "type": "json"}
            ],
            "indexes": [{"name": "orders_status", "keys": [{"path": "status"}]}],
            "rules": {"list_rule": SIGNED_IN, "view_rule": SIGNED_IN, "create_rule": SIGNED_IN, "update_rule": null, "delete_rule": null}
        }),
    )?;
    f.function(
        p,
        "hello",
        "description = \"Says hello over HTTP\"\naccess = \"public\"\nentry = \"index.ts\"\ncapabilities = [\"log\"]\n",
        HELLO,
    )?;
    f.function(
        p,
        "order-created",
        "description = \"Runs after an order is created\"\naccess = \"permission\"\nentry = \"index.ts\"\ncapabilities = [\"log\", \"secrets\", \"fetch\"]\nfetch_hosts = [\"hooks.slack.com\"]\n\n[limits]\ntimeout_ms = 10000\n\n# Secret name = environment variable that holds its value when you apply.\n[secrets]\nSLACK_WEBHOOK_URL = \"SLACK_WEBHOOK_URL\"\n\n[[hooks]]\ncollection = \"orders\"\nevents = [\"after_create\"]\n",
        ORDER_CREATED,
    )?;
    f.resource(
        p,
        Kind::Topic,
        "order-events",
        json!({"labels": {"app": "orders"}, "source": {"collection": "orders", "ops": ["create", "update"]}, "retention_ms": 604_800_000}),
    )?;
    f.resource(
        p,
        Kind::Subscription,
        "order-events-audit",
        json!({"topic": "order-events", "ack_deadline_ms": 30_000, "max_delivery_attempts": 5, "labels": {"app": "orders"}, "start": "earliest"}),
    )?;
    f.seed(
        p,
        "orders",
        &[
            json!({"customer": "ada@example.com", "total": 42.5, "status": "paid", "items": [{"sku": "book", "qty": 1}]}),
            json!({"customer": "linus@example.com", "total": 9, "status": "pending", "items": []}),
        ],
    )
}

fn chat(f: &mut Files, p: &str) -> anyhow::Result<()> {
    f.resource(
        p,
        Kind::Collection,
        "rooms",
        json!({
            "type": "base",
            "fields": [
                {"name": "title", "type": "text", "required": true, "unique": true, "max_length": 80},
                {"name": "topic", "type": "text", "max_length": 500}
            ],
            "indexes": [],
            "rules": {"list_rule": SIGNED_IN, "view_rule": SIGNED_IN, "create_rule": SIGNED_IN, "update_rule": null, "delete_rule": null}
        }),
    )?;
    f.resource(
        p,
        Kind::Collection,
        "messages",
        json!({
            "type": "base",
            "fields": [
                {"name": "room", "type": "relation", "collection": "rooms", "required": true, "cascade_delete": true},
                {"name": "author", "type": "text", "required": true, "max_length": 64},
                {"name": "body", "type": "text", "required": true, "max_length": 2000}
            ],
            "indexes": [{"name": "messages_room_created", "keys": [{"path": "room"}, {"path": "created", "desc": true}]}],
            "rules": {"list_rule": SIGNED_IN, "view_rule": SIGNED_IN, "create_rule": SIGNED_IN, "update_rule": null, "delete_rule": null}
        }),
    )?;
    f.function(
        p,
        "moderate",
        "description = \"Rejects messages with blocked words\"\naccess = \"permission\"\nentry = \"index.ts\"\ncapabilities = [\"log\"]\n\n[limits]\ntimeout_ms = 2000\n\n[[hooks]]\ncollection = \"messages\"\nevents = [\"before_create\"]\n",
        MODERATE,
    )?;
    f.seed(
        p,
        "rooms",
        &[
            json!({"title": "general", "topic": "Anything goes"}),
            json!({"title": "random", "topic": "Off topic"}),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_loads_as_a_valid_project() {
        for t in [Template::Basic, Template::Functions, Template::RealtimeChat] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("app");
            let o = Options {
                dir: dir.clone(),
                template: t,
                name: None,
                project_dir: "forgebase".into(),
                force: false,
            };
            let files = create(&o, &mut Vec::new()).unwrap();
            assert!(files.iter().any(|f| f.ends_with(".github/workflows/forgebase.yml")));
            let p = layout::load(&dir.join("forgebase")).unwrap_or_else(|e| panic!("{t:?}: {e:#}"));
            assert!(p.resources.iter().any(|r| r.kind == Kind::Collection), "{t:?}");
            assert!(p.resources.iter().any(|r| r.kind == Kind::Function), "{t:?}");
            assert!(!p.seeds.is_empty(), "{t:?}");
            assert!(p.warnings.is_empty(), "{t:?}: {:?}", p.warnings);
            let wf = std::fs::read_to_string(dir.join(".github/workflows/forgebase.yml")).unwrap();
            assert!(
                wf.contains("forgebase project plan --dir forgebase")
                    && wf.contains("forgebase deploy --dir forgebase --auto-approve")
            );
            assert!(!wf.contains("{{DIR}}") && !wf.contains("{{VERSION}}"));
            assert!(create(&o, &mut Vec::new()).unwrap_err().to_string().contains("--force"));
        }
    }
}
