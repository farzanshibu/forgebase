//! Label and edge-type rules (FB-139, rls-coverage G10).
//!
//! A node's resource is its label (`graph_label`), an edge's its type
//! (`graph_edge_type`); both are asked with [`Action::GraphRead`] or
//! [`Action::GraphWrite`] and the operation of the call (FB-134). A rule
//! that comes back as a predicate reads the row's own columns: `id`, `label`,
//! `properties` for nodes; `id`, `type`, `source`, `target`, `properties`
//! for edges (the kinds' default [`ResourceSchema`]s, which are exactly the
//! table's column names). [`Rules`] turns the decisions for every label (or
//! type) of a tenant into one SQL condition, so traversal, neighbours,
//! shortest paths and listings filter in the statement itself; an edge is
//! visible only when its type allows it and both of its nodes are visible,
//! and a walk never steps across a hidden edge or into a hidden node.
//!
//! [`Action::GraphRead`]: forgebase_data::Action::GraphRead
//! [`Action::GraphWrite`]: forgebase_data::Action::GraphWrite

use forgebase_data::query::{QueryError, SqlBuilder};
use forgebase_data::{Predicate, ResourceKind, ResourceSchema};

/// The decision for one label or edge type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum One {
    Allow,
    Filter(Predicate),
    Deny,
}

/// The decisions of one caller for the labels (or edge types) of a graph,
/// for one operation.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    kind: ResourceKind,
    unrestricted: bool,
    any: bool,
    allow: Vec<String>,
    filter: Vec<(String, Predicate)>,
}

impl Rules {
    /// No restriction (privileged callers, or every name allowed).
    pub(crate) fn unrestricted(kind: ResourceKind) -> Self {
        Self {
            kind,
            unrestricted: true,
            any: true,
            allow: Vec::new(),
            filter: Vec::new(),
        }
    }

    /// A graph without any name of this kind yet: nothing to show, but
    /// `probe` (the decision for the name `*`, which the rules and keys of
    /// every name match) says whether the caller may read the kind at all.
    pub(crate) fn empty(kind: ResourceKind, probe: &One) -> Self {
        Self {
            kind,
            unrestricted: false,
            any: *probe != One::Deny,
            allow: Vec::new(),
            filter: Vec::new(),
        }
    }

    /// From the decision of every name. Names left out are hidden.
    pub(crate) fn from_decisions(kind: ResourceKind, decisions: Vec<(String, One)>) -> Self {
        let mut r = Self {
            kind,
            unrestricted: decisions.iter().all(|(_, d)| *d == One::Allow),
            any: false,
            allow: Vec::new(),
            filter: Vec::new(),
        };
        for (name, d) in decisions {
            match d {
                One::Allow => r.allow.push(name),
                One::Filter(p) => r.filter.push((name, p)),
                One::Deny => {}
            }
        }
        r.any = r.unrestricted || !r.allow.is_empty() || !r.filter.is_empty();
        r
    }

    /// Restricted rules from given parts (FB-163: the part of a caller's
    /// rules the derived graph applies by row id).
    pub(crate) fn partial(kind: ResourceKind, allow: Vec<String>, filter: Vec<(String, Predicate)>) -> Self {
        Self {
            kind,
            unrestricted: false,
            any: !allow.is_empty() || !filter.is_empty(),
            allow,
            filter,
        }
    }

    /// The kind (labels or edge types).
    pub(crate) fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Names allowed without a predicate (empty when unrestricted).
    pub(crate) fn allowed(&self) -> &[String] {
        &self.allow
    }

    /// Names allowed under a predicate.
    pub(crate) fn filters(&self) -> &[(String, Predicate)] {
        &self.filter
    }

    /// Whether every name is allowed without a predicate.
    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted
    }

    /// Whether some name is allowed (with or without a predicate).
    pub fn allows_any(&self) -> bool {
        self.any
    }

    /// The schema the predicates read.
    pub fn schema(&self) -> ResourceSchema {
        self.kind.default_schema()
    }

    /// The boolean SQL condition over a node (or edge) row aliased `alias`,
    /// `None` when unrestricted. Parameters are bound into `b`; the
    /// placeholders are numbered, so the condition may be repeated in one
    /// statement.
    pub fn compile(&self, b: &mut SqlBuilder<'_>, alias: &str) -> Result<Option<String>, QueryError> {
        if self.unrestricted {
            return Ok(None);
        }
        let column = match self.kind {
            ResourceKind::GraphEdgeType => "type",
            _ => "label",
        };
        let col = format!("{alias}.{}", b.ident(column));
        let mut parts = Vec::new();
        if !self.allow.is_empty() {
            let list: Vec<String> = self.allow.iter().map(|n| b.bind(n.clone())).collect();
            parts.push(format!("{col} IN ({})", list.join(", ")));
        }
        let schema = self.schema();
        for (name, p) in &self.filter {
            let n = b.bind(name.clone());
            let cond = schema.compile(b, alias, p)?;
            parts.push(format!("({col} = {n} AND ({cond}))"));
        }
        Ok(Some(if parts.is_empty() {
            "1 = 0".to_owned()
        } else {
            format!("({})", parts.join(" OR "))
        }))
    }
}

/// What one caller may see (or change) of a graph for one operation: the
/// node rules by label and the edge rules by type. Public so engines that
/// walk the graph with their own SQL (FB-147, FB-163 openCypher on AGE) can
/// tell whether a rule applies and compile the same conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphAccess {
    /// Node rules by label.
    pub nodes: Rules,
    /// Edge rules by type.
    pub edges: Rules,
}

impl GraphAccess {
    /// No restriction.
    pub(crate) fn unrestricted() -> Self {
        Self {
            nodes: Rules::unrestricted(ResourceKind::GraphLabel),
            edges: Rules::unrestricted(ResourceKind::GraphEdgeType),
        }
    }

    /// Whether no label or edge-type rule restricts the caller.
    pub fn is_unrestricted(&self) -> bool {
        self.nodes.is_unrestricted() && self.edges.is_unrestricted()
    }

    /// The visibility condition of a node row aliased `alias`.
    pub fn node_sql(&self, b: &mut SqlBuilder<'_>, alias: &str) -> Result<Option<String>, QueryError> {
        self.nodes.compile(b, alias)
    }

    /// The visibility condition of an edge row aliased `alias` in table
    /// `nodes_table`: its type rule, and both of its nodes visible.
    pub fn edge_sql(
        &self,
        b: &mut SqlBuilder<'_>,
        alias: &str,
        nodes_table: &str,
    ) -> Result<Option<String>, QueryError> {
        let mut parts = Vec::new();
        if let Some(c) = self.edges.compile(b, alias)? {
            parts.push(c);
        }
        if let Some(n) = self.nodes.compile(b, "vn")? {
            for end in ["source", "target"] {
                parts.push(format!(
                    "EXISTS (SELECT 1 FROM {nodes_table} vn WHERE vn.tenant = {alias}.tenant \
                     AND vn.id = {alias}.{end} AND {n})"
                ));
            }
        }
        Ok((!parts.is_empty()).then(|| parts.join(" AND ")))
    }
}

/// ` AND (<cond>)`, or nothing.
pub(crate) fn and(cond: Option<&str>) -> String {
    cond.map(|c| format!(" AND ({c})")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_db::SqliteDialect;
    use std::collections::HashMap;

    #[test]
    fn decisions_compile_to_one_condition() {
        let d = SqliteDialect;
        let p = Predicate::parse("properties.team = 'a'", HashMap::new()).unwrap();
        let r = Rules::from_decisions(
            ResourceKind::GraphLabel,
            vec![
                ("person".into(), One::Allow),
                ("doc".into(), One::Filter(p)),
                ("secret".into(), One::Deny),
            ],
        );
        assert!(!r.is_unrestricted() && r.allows_any());
        let mut b = SqlBuilder::new(&d);
        let sql = r.compile(&mut b, "n").unwrap().unwrap();
        assert!(
            sql.contains("n.\"label\" IN (?1)") && sql.contains("n.\"properties\""),
            "{sql}"
        );
        assert!(!sql.contains("secret"), "denied labels are simply absent: {sql}");
        let all = Rules::from_decisions(ResourceKind::GraphLabel, vec![("a".into(), One::Allow)]);
        assert!(all.compile(&mut b, "n").unwrap().is_none());
        let none = Rules::from_decisions(ResourceKind::GraphLabel, vec![("a".into(), One::Deny)]);
        assert_eq!(none.compile(&mut b, "n").unwrap().as_deref(), Some("1 = 0"));
        assert!(!none.allows_any());
    }

    #[test]
    fn edges_need_their_type_and_both_nodes() {
        let d = SqliteDialect;
        let acc = GraphAccess {
            nodes: Rules::from_decisions(ResourceKind::GraphLabel, vec![("a".into(), One::Deny)]),
            edges: Rules::unrestricted(ResourceKind::GraphEdgeType),
        };
        let mut b = SqlBuilder::new(&d);
        let sql = acc.edge_sql(&mut b, "e", "nodes").unwrap().unwrap();
        assert!(sql.contains("e.source") && sql.contains("e.target"), "{sql}");
        assert!(
            GraphAccess::unrestricted()
                .edge_sql(&mut b, "e", "x")
                .unwrap()
                .is_none()
        );
    }
}
