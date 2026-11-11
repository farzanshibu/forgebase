//! Policy generation (rls-coverage §3.4): from a collection, its rules and
//! field rules to the SQL that installs them. Pure: [`generate`] returns
//! statements, [`apply`] runs them in the transaction of the node to write
//! to (FB-173 applies the same statements on every shard).

use std::collections::HashMap;
use std::sync::Arc;

use forgebase_data::{TENANT_COLUMN, is_tenant_scoped};
use forgebase_db::{PgDialect, Statement, WriteTx};
use forgebase_query::{CompileOptions, Compiler, QueryError, SqlBuilder};
use forgebase_schema::{Collection, CollectionType, FieldKind};

use super::render::{PolicyRender, RelFn, dollar_tag, ident, literal, short_hash};
use super::{LOGIN_GROUP, NativeMode, RLS_ROLE};
use crate::snapshot::{CollectionPolicy, RuleSpec};

/// The statements for one collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Generated {
    /// SQL, in order.
    pub statements: Vec<String>,
    /// Every rule and field rule compiled natively. When `false` the
    /// rules that did not compile are closed (`false`) for direct logins,
    /// and `enforce` mode keeps that collection on the data core's checks
    /// alone (the API path never sees fewer rows than the rules allow).
    pub complete: bool,
    /// Why a rule did not compile natively.
    pub problems: Vec<String>,
}

/// Collections that get native policies: base and auth collections (views
/// read their tables with the caller's rights, `security_invoker`).
pub fn has_policies(coll: &Collection) -> bool {
    coll.kind != CollectionType::View
}

fn table(coll: &Collection) -> String {
    format!("public.{}", ident(&coll.name))
}

fn view(coll: &Collection) -> String {
    format!("api.{}", ident(&coll.name))
}

fn trigger_fn(coll: &Collection) -> String {
    format!("api.\"fb_w_{}\"", short_hash(&[&coll.name]))
}

/// Statements that remove the `api` view and its write trigger function
/// (before DDL that the view would block, and before regeneration).
pub fn drop_objects(coll: &Collection) -> Vec<String> {
    vec![
        format!("DROP VIEW IF EXISTS {} CASCADE", view(coll)),
        format!("DROP FUNCTION IF EXISTS {}()", trigger_fn(coll)),
    ]
}

/// Drops every `fb_*` policy of the table (generated names only).
fn drop_policies(coll: &Collection) -> String {
    let body = format!(
        "DECLARE p record; BEGIN \
         FOR p IN SELECT policyname FROM pg_catalog.pg_policies \
           WHERE schemaname = 'public' AND tablename = {name} AND policyname LIKE 'fb\\_%' LOOP \
           EXECUTE format('DROP POLICY %I ON public.%I', p.policyname, {name}); \
         END LOOP; END",
        name = literal(&coll.name)
    );
    let tag = dollar_tag(&body);
    format!("DO {tag}{body}{tag}")
}

/// Drops `fb_rel_*` functions no policy depends on any more.
pub fn drop_unused_relations() -> String {
    let body = "DECLARE f record; BEGIN \
         FOR f IN SELECT p.oid::regprocedure AS sig FROM pg_catalog.pg_proc p \
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
           WHERE n.nspname = 'forgebase' AND p.proname LIKE 'fb\\_rel\\_%' \
             AND pg_catalog.pg_has_role(current_user, p.proowner, 'USAGE') \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend d \
                             WHERE d.refclassid = 'pg_catalog.pg_proc'::regclass AND d.refobjid = p.oid) LOOP \
           EXECUTE format('DROP FUNCTION %s', f.sig); \
         END LOOP; END";
    format!("DO $fb${body}$fb$")
}

/// One compiled rule: SQL plus the relation functions it calls.
struct Compiled {
    sql: String,
    rels: Vec<RelFn>,
}

fn compile_rule(
    spec: &RuleSpec,
    coll: &Collection,
    catalog: &HashMap<String, Collection>,
) -> Result<Compiled, QueryError> {
    let rule = match spec {
        RuleSpec::Public => {
            return Ok(Compiled {
                sql: "true".into(),
                rels: Vec::new(),
            });
        }
        RuleSpec::Expr(r) => r,
    };
    let render = Arc::new(PolicyRender::new(rule.refs().to_vec(), is_tenant_scoped(coll)));
    let opts = CompileOptions {
        native: Some(render.clone()),
        ..CompileOptions::default()
    };
    let mut b = SqlBuilder::new(&PgDialect);
    let sql = Compiler::new(catalog, &opts).filter(&mut b, coll, "", rule.expr())?;
    if !b.params().is_empty() {
        return Err(QueryError::InvalidOperation("the rule needs bound values".into()));
    }
    Ok(Compiled {
        sql,
        rels: render.take_relations(),
    })
}

/// Generates the native row-level security of `coll` for `mode`.
///
/// Every mode: `ENABLE` and `FORCE ROW LEVEL SECURITY` and the old `fb_*`
/// policies dropped. `off` stops there (no policy: the RLS role sees
/// nothing). `logins` and `enforce` add the grants to `forgebase_rls`, the
/// policies of the table in rls-coverage §3.4 and the `api` view with its
/// `INSTEAD OF` write trigger for direct logins.
pub fn generate(
    coll: &Collection,
    policy: Option<&CollectionPolicy>,
    catalog: &[Collection],
    mode: NativeMode,
) -> Generated {
    let mut out = Generated {
        complete: true,
        ..Generated::default()
    };
    if !has_policies(coll) {
        return out;
    }
    let t = table(coll);
    let s = &mut out.statements;
    s.extend(drop_objects(coll));
    s.push(format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY"));
    s.push(format!("ALTER TABLE {t} FORCE ROW LEVEL SECURITY"));
    s.push(drop_policies(coll));
    if mode == NativeMode::Off {
        s.push(format!("REVOKE ALL ON {t} FROM {RLS_ROLE}"));
        s.push(drop_unused_relations());
        return out;
    }
    let by_id: HashMap<String, Collection> = catalog.iter().map(|c| (c.id.clone(), c.clone())).collect();
    let scoped = is_tenant_scoped(coll);
    let name = literal(&coll.name);
    let sc = if scoped { "true" } else { "false" };
    let read = "(SELECT forgebase.read_action())";
    s.push(format!("GRANT SELECT, INSERT, UPDATE, DELETE ON {t} TO {RLS_ROLE}"));
    let mut policy_stmts = Vec::new();
    let mut add = |n: &str, restrictive: bool, cmd: &str, using: Option<String>, check: Option<String>| {
        let mut p = format!(
            "CREATE POLICY {n} ON {t} AS {} FOR {cmd}",
            if restrictive { "RESTRICTIVE" } else { "PERMISSIVE" }
        );
        if let Some(u) = using {
            p.push_str(&format!(" USING ({u})"));
        }
        if let Some(c) = check {
            p.push_str(&format!(" WITH CHECK ({c})"));
        }
        policy_stmts.push(p);
    };
    // Restrictive: tenancy, the gate (steps decided in Rust, and deny wins),
    // record ACL denials.
    if scoped {
        let e = format!("{} = (SELECT forgebase.tenant())", ident(TENANT_COLUMN));
        add("fb_tenant", true, "ALL", Some(e.clone()), Some(e));
    }
    let gate = |action: &str| {
        format!("(SELECT forgebase.gate({sc})) AND (SELECT forgebase.perm({name}, {action}, {sc})) <> 'denied'")
    };
    add("fb_gate_select", true, "SELECT", Some(gate("forgebase.read_action()")), None);
    add("fb_gate_insert", true, "INSERT", None, Some(gate("'create'")));
    add("fb_gate_update", true, "UPDATE", Some(gate("'update'")), Some(gate("'update'")));
    add("fb_gate_delete", true, "DELETE", Some(gate("'delete'")), None);
    let acl = |action: &str, part: &str| format!("\"id\" = ANY ((SELECT (forgebase.acl({name}, {action})).{part})::text[])");
    add(
        "fb_acl_deny_select",
        true,
        "SELECT",
        Some(format!("NOT ({})", acl("forgebase.read_action()", "deny"))),
        None,
    );
    add(
        "fb_acl_deny_update",
        true,
        "UPDATE",
        Some(format!("NOT ({})", acl("'update'", "deny"))),
        None,
    );
    add(
        "fb_acl_deny_delete",
        true,
        "DELETE",
        Some(format!("NOT ({})", acl("'delete'", "deny"))),
        None,
    );
    // Permissive: a permission grants, else a rule, else an ACL entry.
    let perm = |action: &str| format!("(SELECT forgebase.perm({name}, {action}, {sc})) = 'granted'");
    add(
        "fb_write_select",
        false,
        "SELECT",
        Some(format!("{read} IN ('create', 'update', 'delete')")),
        None,
    );
    add("fb_perm_select", false, "SELECT", Some(perm("forgebase.read_action()")), None);
    add("fb_perm_insert", false, "INSERT", None, Some(perm("'create'")));
    add("fb_perm_update", false, "UPDATE", Some(perm("'update'")), None);
    add("fb_perm_delete", false, "DELETE", Some(perm("'delete'")), None);
    add(
        "fb_acl_select",
        false,
        "SELECT",
        Some(acl("forgebase.read_action()", "allow")),
        None,
    );
    add("fb_acl_update", false, "UPDATE", Some(acl("'update'", "allow")), None);
    add("fb_acl_delete", false, "DELETE", Some(acl("'delete'", "allow")), None);

    let mut rels: Vec<RelFn> = Vec::new();
    let mut problems = Vec::new();
    let mut rule = |i: usize| -> Option<String> {
        let spec = policy.and_then(|p| p.rules[i].as_ref())?;
        match compile_rule(spec, coll, &by_id) {
            Ok(c) => {
                for r in c.rels {
                    if !rels.iter().any(|x| x.name == r.name) {
                        rels.push(r);
                    }
                }
                Some(c.sql)
            }
            Err(e) => {
                problems.push(format!("{} rule: {e}", ["list", "view", "create", "update", "delete"][i]));
                Some("false".into())
            }
        }
    };
    let (list, view_rule, create, update, delete) = (rule(0), rule(1), rule(2), rule(3), rule(4));
    let reads: Vec<String> = [("list", &list), ("view", &view_rule)]
        .into_iter()
        .filter_map(|(a, r)| r.as_ref().map(|r| format!("({read} = '{a}' AND ({r}))")))
        .collect();
    if !reads.is_empty() {
        add("fb_rule_select", false, "SELECT", Some(reads.join(" OR ")), None);
    }
    if let Some(r) = &create {
        add("fb_rule_insert", false, "INSERT", None, Some(r.clone()));
    }
    if let Some(r) = &update {
        add("fb_rule_update", false, "UPDATE", Some(r.clone()), Some(r.clone()));
    }
    if let Some(r) = &delete {
        add("fb_rule_delete", false, "DELETE", Some(r.clone()), None);
    }

    // The `api` view for direct logins: columns nobody may read are left
    // out; request-dependent field read rules mask the value.
    let mut cols: Vec<String> = ["id", "created", "updated", "_version", "_data"]
        .iter()
        .map(|c| ident(c))
        .collect();
    if scoped {
        cols.push(ident(TENANT_COLUMN));
    }
    let mut writable: Vec<(String, Option<String>)> = Vec::new();
    for f in &coll.fields {
        let secret = matches!(f.kind, FieldKind::Password { .. }) || f.hidden;
        if secret {
            continue;
        }
        let rules = policy.and_then(|p| p.fields.get(&f.name.to_ascii_lowercase()));
        let mut field_rule = |spec: Option<&RuleSpec>| -> Option<String> {
            let spec = spec?;
            Some(match compile_rule(spec, coll, &by_id) {
                Ok(c) if c.rels.is_empty() => c.sql,
                Ok(_) => "false".into(),
                Err(e) => {
                    problems.push(format!("field rule of {}: {e}", f.name));
                    "false".into()
                }
            })
        };
        let read = field_rule(rules.and_then(|r| r.read.as_ref()));
        let write = field_rule(rules.and_then(|r| r.write.as_ref()));
        let c = ident(&f.name);
        cols.push(match read {
            None => c.clone(),
            Some(r) => format!("CASE WHEN coalesce(({r}), false) THEN {c} END AS {c}"),
        });
        writable.push((f.name.clone(), write));
    }
    out.complete = problems.is_empty();
    out.problems = problems;
    for r in rels {
        out.statements.extend(r.statements);
    }
    out.statements.extend(policy_stmts);
    let s = &mut out.statements;
    let v = view(coll);
    s.push(format!(
        "CREATE VIEW {v} WITH (security_barrier = true) AS SELECT {} FROM {t}",
        cols.join(", ")
    ));
    s.push(format!("ALTER VIEW {v} OWNER TO {RLS_ROLE}"));
    s.extend(write_trigger(coll, &writable));
    let grant_body = format!(
        "BEGIN IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = '{LOGIN_GROUP}') THEN \
         GRANT SELECT, INSERT, UPDATE, DELETE ON {v} TO {LOGIN_GROUP}; END IF; END"
    );
    let tag = dollar_tag(&grant_body);
    s.push(format!("DO {tag}{grant_body}{tag}"));
    s.push(drop_unused_relations());
    out
}

/// The `INSTEAD OF` trigger of the `api` view: writes go to the table as
/// `forgebase_rls` (so the table's policies check them); fields the caller
/// may not write are refused; system fields are set here, never by the
/// caller. Direct writes skip the data core: no validation beyond the
/// column types, no change events, no record versions.
fn write_trigger(coll: &Collection, writable: &[(String, Option<String>)]) -> Vec<String> {
    let t = table(coll);
    let f = trigger_fn(coll);
    let refuse = |name: &str, rule: &Option<String>, changed: &str| -> String {
        match rule {
            None => String::new(),
            Some(r) => format!(
                "IF {changed} AND NOT coalesce(({r}), false) THEN \
                 RAISE EXCEPTION 'field % is read-only for this login', {} USING ERRCODE = '42501'; END IF; ",
                literal(name)
            ),
        }
    };
    let mut ins_cols = vec![
        ident("id"),
        ident("created"),
        ident("updated"),
        ident("_version"),
        ident("_data"),
    ];
    let mut ins_vals = vec![
        "coalesce(NEW.\"id\", substr(md5(random()::text || clock_timestamp()::text), 1, 15))".to_owned(),
        "forgebase.now_text()".into(),
        "forgebase.now_text()".into(),
        "1".into(),
        "NEW.\"_data\"".into(),
    ];
    if is_tenant_scoped(coll) {
        ins_cols.push(ident(TENANT_COLUMN));
        ins_vals.push("forgebase.tenant()".into());
    }
    let mut checks_ins = String::new();
    let mut checks_upd = String::new();
    let mut sets = vec![
        "\"updated\" = forgebase.now_text()".to_owned(),
        "\"_version\" = \"_version\" + 1".into(),
        "\"_data\" = NEW.\"_data\"".into(),
    ];
    for (name, rule) in writable {
        let c = ident(name);
        ins_cols.push(c.clone());
        ins_vals.push(format!("NEW.{c}"));
        checks_ins.push_str(&refuse(name, rule, &format!("NEW.{c} IS NOT NULL")));
        checks_upd.push_str(&refuse(name, rule, &format!("NEW.{c} IS DISTINCT FROM OLD.{c}")));
        // A masked column reads as NULL: only a changed value is written.
        sets.push(format!("{c} = CASE WHEN NEW.{c} IS DISTINCT FROM OLD.{c} THEN NEW.{c} ELSE {c} END"));
    }
    let body = format!(
        "BEGIN \
         IF TG_OP = 'DELETE' THEN \
           DELETE FROM {t} WHERE \"id\" = OLD.\"id\"; \
           IF NOT FOUND THEN RETURN NULL; END IF; \
           RETURN OLD; \
         ELSIF TG_OP = 'INSERT' THEN \
           {checks_ins}\
           INSERT INTO {t} ({}) VALUES ({}); \
           RETURN NEW; \
         END IF; \
         IF NEW.\"id\" IS DISTINCT FROM OLD.\"id\" THEN \
           RAISE EXCEPTION 'the id cannot change' USING ERRCODE = '42501'; \
         END IF; \
         {checks_upd}\
         UPDATE {t} SET {} WHERE \"id\" = OLD.\"id\"; \
         IF NOT FOUND THEN RETURN NULL; END IF; \
         RETURN NEW; \
         END",
        ins_cols.join(", "),
        ins_vals.join(", "),
        sets.join(", ")
    );
    let tag = dollar_tag(&body);
    let v = view(coll);
    vec![
        format!(
            "CREATE FUNCTION {f}() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER \
             SET search_path = forgebase, public AS {tag}{body}{tag}"
        ),
        format!("ALTER FUNCTION {f}() OWNER TO {RLS_ROLE}"),
        format!("REVOKE ALL ON FUNCTION {f}() FROM PUBLIC"),
        format!("CREATE TRIGGER fb_write INSTEAD OF INSERT OR UPDATE OR DELETE ON {v} FOR EACH ROW EXECUTE FUNCTION {f}()"),
    ]
}

/// Runs generated statements in `tx` (the transaction of the node to write
/// to).
pub async fn apply(tx: &mut dyn WriteTx, statements: &[String]) -> Result<(), forgebase_db::DbError> {
    for s in statements {
        tx.execute(&Statement::raw(s.clone())).await?;
    }
    Ok(())
}
