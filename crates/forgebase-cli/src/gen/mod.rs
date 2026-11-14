//! `forgebase gen types`: typed models of a project's collections (ADR-0038).
//!
//! The collections come from a project directory (`--from-project`, no
//! server needed) or from a running server (`--url`); both are turned into
//! the same canonical specs (ADR-0037 §4) and then into one language-neutral
//! model that the emitters ([`ts`], [`rust`]) print. The output is
//! deterministic: collections sorted by name, fields in declared order, no
//! timestamps or paths. `--check` compares it with the file on disk and exits
//! 2 when it is stale, so CI can require generated types to be committed.

pub mod model;
pub mod rust;
pub mod ts;

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Args, Subcommand, ValueEnum};

use crate::GlobalArgs;
use crate::project::layout;
use crate::project::model::Kind;
use crate::project::remote::{Connection, Remote};

/// Exit code of `--check` when the file is stale (`plan` and `drift` use 2 for "differs" too).
const EXIT_STALE: u8 = 2;

/// `forgebase gen`.
#[derive(Debug, Args)]
pub struct GenCli {
    #[command(subcommand)]
    command: GenCommand,
}

#[derive(Debug, Subcommand)]
enum GenCommand {
    /// Typed models of every collection: TypeScript (for `@forgebase/sdk`) or Rust (serde structs).
    Types(TypesArgs),
}

/// Output language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Lang {
    /// TypeScript types for `fb.collection<T>(name)`.
    Ts,
    /// Rust serde structs for the Rust SDK's `Records<T>`.
    Rust,
}

#[derive(Debug, Args)]
struct TypesArgs {
    /// Output language.
    #[arg(long, value_enum)]
    lang: Lang,
    /// Read the collections from a project directory (ADR-0037 layout) [default: ./forgebase when it exists].
    #[arg(long, value_name = "DIR", conflicts_with = "url")]
    from_project: Option<PathBuf>,
    /// Read the collections from a running server instead.
    #[arg(long, value_name = "URL")]
    url: Option<String>,
    /// API key or personal access token for --url [env: FORGEBASE_TOKEN]
    #[arg(long, value_name = "TOKEN", requires = "url")]
    token: Option<String>,
    /// Write the types to FILE [default: standard output].
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Do not write: exit 2 if FILE (--out) is missing or differs from what would be generated.
    #[arg(long, requires = "out")]
    check: bool,
}

/// Runs `forgebase gen`.
pub async fn run(g: &GlobalArgs, cli: GenCli) -> anyhow::Result<ExitCode> {
    let GenCommand::Types(a) = cli.command;
    types(g, a).await
}

async fn specs_from_project(dir: &Path) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let project = layout::load(dir)?;
    crate::project::warn(&project);
    Ok(project
        .resources
        .iter()
        .filter(|r| r.kind == Kind::Collection)
        .map(|r| (r.name.clone(), r.spec.clone()))
        .collect())
}

async fn specs_from_server(
    g: &GlobalArgs,
    url: &str,
    token: Option<String>,
) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
    let remote = Remote::connect(
        &Connection {
            server: Some(url.to_owned()),
            token,
            ..Connection::default()
        },
        &layout::Target::default(),
        g,
    )
    .await?;
    if !remote.has_credential() {
        eprintln!("warning: no credential: set FORGEBASE_TOKEN (or pass --token); listing collections needs one");
    }
    let kinds: BTreeSet<Kind> = [Kind::Collection].into();
    let all = remote.fetch_all(&kinds).await?;
    Ok(all
        .into_values()
        .filter(|a| a.kind == Kind::Collection)
        .map(|a| (a.name, a.spec))
        .collect())
}

/// The generated text of a language for a model.
fn render(lang: Lang, model: &model::Model) -> String {
    match lang {
        Lang::Ts => ts::emit(model),
        Lang::Rust => rust::emit(model),
    }
}

async fn types(g: &GlobalArgs, a: TypesArgs) -> anyhow::Result<ExitCode> {
    let specs = match (&a.url, &a.from_project) {
        (Some(url), _) => specs_from_server(g, url, a.token.clone()).await?,
        (None, Some(dir)) => specs_from_project(dir).await?,
        (None, None) => {
            let dir = Path::new("forgebase");
            if !dir.join(layout::PROJECT_FILE).is_file() {
                bail!(
                    "no project to read: pass --from-project DIR (a directory with {}) or --url SERVER",
                    layout::PROJECT_FILE
                );
            }
            specs_from_project(dir).await?
        }
    };
    let model = model::build(&specs);
    let text = render(a.lang, &model);
    let n = model.collections.len();
    match (&a.out, a.check) {
        (None, _) => {
            std::io::stdout().write_all(text.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        (Some(path), true) => {
            let have = std::fs::read_to_string(path).ok();
            if have.as_deref() == Some(text.as_str()) {
                println!("{} is up to date ({n} collection{})", path.display(), plural(n));
                Ok(ExitCode::SUCCESS)
            } else {
                let why = if have.is_some() {
                    "differs from"
                } else {
                    "is missing; it would be"
                };
                eprintln!(
                    "{} {why} the generated types: run `forgebase gen types --lang {} --out {}`",
                    path.display(),
                    match a.lang {
                        Lang::Ts => "ts",
                        Lang::Rust => "rust",
                    },
                    path.display()
                );
                Ok(ExitCode::from(EXIT_STALE))
            }
        }
        (Some(path), false) => {
            if std::fs::read_to_string(path).ok().as_deref() == Some(text.as_str()) {
                println!("{} is up to date ({n} collection{})", path.display(), plural(n));
                return Ok(ExitCode::SUCCESS);
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(path, &text).with_context(|| format!("cannot write {}", path.display()))?;
            println!("wrote {} ({n} collection{})", path.display(), plural(n));
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
