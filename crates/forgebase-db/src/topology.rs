//! The database topology: sets, nodes and tenant placements (ADR-0029 §3.1,
//! §3.3, §3.4, §4.2).
//!
//! * A **set** ([`DbSet`]) is one primary plus its replicas. The control set
//!   (`id = "control"`, number 0) always exists; shards are numbered 1 …
//!   [`MAX_SET_NUMBER`]. A single-server deployment is the control set alone,
//!   hosting every tenant.
//! * A **node** ([`DbNode`]) is one PostgreSQL server of a set. Control's
//!   primary may have no row: it is the database the server opened.
//! * A **placement** ([`Placement`]) says which set holds a tenant
//!   (environment id). A tenant without a row lives on control. Its `epoch`
//!   grows by one on every change, so a cached copy can tell it is stale.
//!
//! The model is plain data with `serde` on every type: the admin API (FB-175)
//! and the dashboard's topology screen (FB-176) show and edit it, and
//! `<data_dir>/topology.json` ([`write_topology_file`]) is the same
//! [`Topology`] serialised, so a restart finds the control primary even after
//! a promote. Every enum is stored as its snake-case text in the control
//! tables (`migrations/*/0033_db_topology.sql`), with a matching `CHECK`.
//!
//! The store functions here run on the control set's [`Backend`] and are the
//! only writers of these tables; [`crate::router::DbRouter`] caches what they
//! write. Topology changes bump one counter, the **topology epoch**
//! (`_forgebase_meta` key [`EPOCH_META_KEY`] on control), in the same
//! transaction.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::backend::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use crate::{BackendKind, Database, DbError};

/// Id of the control set.
pub const CONTROL_SET_ID: &str = "control";
/// Highest set number: 8 191 sets (0 … 8 190), so every identity value stays
/// below 2⁵³ and exact in JavaScript (ADR-0029 §3.3, §3.12).
pub const MAX_SET_NUMBER: u16 = 8190;
/// Each set's identity values come from a range of 2^`RANGE_BITS` values.
pub const RANGE_BITS: u32 = 40;
/// `_forgebase_meta` key (on control) of the topology epoch.
pub const EPOCH_META_KEY: &str = "topology_epoch";
/// File name of the topology snapshot in the data directory.
pub const TOPOLOGY_FILE: &str = "topology.json";
/// Format version of [`TopologyFile`].
pub const TOPOLOGY_FILE_VERSION: u32 = 1;
/// Schemas whose sequences [`apply_sequence_range`] confines to a set's range.
pub const SEQUENCE_SCHEMAS: &[&str] = &["forgebase", "public"];

macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The stored text (snake case, as in the table's `CHECK`).
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = DbError;

            fn from_str(s: &str) -> Result<Self, DbError> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(DbError::Statement(format!(
                        concat!("unknown ", stringify!($name), " {:?} in the topology tables"),
                        other
                    ))),
                }
            }
        }
    };
}

text_enum! {
    /// What a set is.
    SetKind {
        /// The control set (number 0): control tables, and tenants not placed elsewhere.
        Control => "control",
        /// A shard (number ≥ 1): tenant data only.
        Shard => "shard",
    }
}

text_enum! {
    /// Lifecycle of a set.
    SetState {
        /// Being added (`database.shard.add`): not routable.
        Provisioning => "provisioning",
        /// Serves its tenants and takes new ones (when `hosts_tenants`).
        Active => "active",
        /// Serves its tenants, takes no new ones; a rebalance moves them out.
        Draining => "draining",
        /// Below the binary's schema head; its tenants get `503 shard_migrating`.
        Migrating => "migrating",
        /// Removed; kept for history. Not routable.
        Retired => "retired",
    }
}

text_enum! {
    /// Where a node runs (ADR-0029 §2.1).
    NodeKind {
        /// Bundled engine on this host, a child process (`postgres/nodes/<name>`).
        Local => "local",
        /// Bundled engine on another host, run by `forgebase node`.
        Remote => "remote",
        /// A server the operator runs, reached by URL.
        External => "external",
    }
}

text_enum! {
    /// A node's role in its set.
    NodeRole {
        /// Takes the set's writes.
        Primary => "primary",
        /// Streams from the primary; serves opt-in reads (FB-167).
        Replica => "replica",
    }
}

text_enum! {
    /// Health and lifecycle of a node (ADR-0029 §2.7).
    NodeState {
        /// Being created or cloned.
        Provisioning => "provisioning",
        /// A primary that answers.
        Ready => "ready",
        /// A replica replaying towards the primary.
        CatchingUp => "catching_up",
        /// A replica within the lag limits.
        Streaming => "streaming",
        /// A replica above the lag limits (out of rotation).
        Lagging => "lagging",
        /// Does not answer.
        Unreachable => "unreachable",
        /// Diverged or restored; must be re-cloned or rewound.
        NeedsRebuild => "needs_rebuild",
        /// Being promoted to primary.
        Promoting => "promoting",
        /// Removed.
        Retired => "retired",
    }
}

text_enum! {
    /// Where a node row came from.
    NodeOrigin {
        /// Added through `/api/system/database/*`.
        Api => "api",
        /// Declared in `forgebase.toml` (`[[database.nodes]]`); read-only in the dashboard.
        Config => "config",
    }
}

text_enum! {
    /// State of a tenant's placement (ADR-0029 §3.4, §3.8).
    PlacementState {
        /// Served by its set.
        Active => "active",
        /// Being copied to another set; still served by `set_id`.
        Moving => "moving",
        /// The move's switch: new requests wait (at most the freeze timeout).
        Frozen => "frozen",
        /// Moved (`set_id` is the target); the source's rows are not deleted yet.
        Cleanup => "cleanup",
    }
}

text_enum! {
    /// State of an online tenant move (`_forgebase_db_moves`, ADR-0029 §3.8).
    MoveState {
        /// Target at schema head, publication created.
        Preparing => "preparing",
        /// Initial copy of the tenant's rows.
        Copying => "copying",
        /// Streaming changes until the lag is small.
        CatchingUp => "catching_up",
        /// The freeze: drain, barrier, renumber, verify, flip.
        Switching => "switching",
        /// Flipped; waiting to delete the source's rows.
        Cleanup => "cleanup",
        /// Finished.
        Done => "done",
        /// Stopped before the flip; nothing visible changed.
        Aborted => "aborted",
    }
}

text_enum! {
    /// State of a two-phase schema change (`_forgebase_xact_journal`, ADR-0029 §3.7).
    XactState {
        /// Every node prepared; `COMMIT PREPARED` is under way.
        Committing => "committing",
        /// Committed on every node.
        Committed => "committed",
    }
}

/// One replica set (`_forgebase_db_sets`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSet {
    /// Stable id (`control` for the control set).
    pub id: String,
    /// Display name, unique ignoring case.
    pub name: String,
    /// Control or shard.
    pub kind: SetKind,
    /// The set number *k*: identity values come from [`sequence_range`]`(k)`.
    pub number: u16,
    /// Lifecycle state.
    pub state: SetState,
    /// Relative capacity for placement (load score = estimated bytes ÷ weight).
    pub weight: u32,
    /// Whether the set takes new tenants (control can stop once drained).
    pub hosts_tenants: bool,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

impl DbSet {
    /// The control set as the migration seeds it (used before the topology
    /// tables can be read, and by [`Topology::single`]).
    pub fn control() -> Self {
        Self {
            id: CONTROL_SET_ID.to_owned(),
            name: CONTROL_SET_ID.to_owned(),
            kind: SetKind::Control,
            number: 0,
            state: SetState::Active,
            weight: 1,
            hosts_tenants: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// Whether this is the control set.
    pub fn is_control(&self) -> bool {
        self.kind == SetKind::Control
    }

    /// Whether requests may be routed to this set (not provisioning or retired).
    pub fn is_routable(&self) -> bool {
        !matches!(self.state, SetState::Provisioning | SetState::Retired)
    }

    /// Whether new tenants may be placed here.
    pub fn takes_new_tenants(&self) -> bool {
        self.state == SetState::Active && self.hosts_tenants
    }

    /// The identity range of this set.
    pub fn sequence_range(&self) -> SequenceRange {
        sequence_range(self.number)
    }
}

/// One server of a set (`_forgebase_db_nodes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbNode {
    /// Stable id.
    pub id: String,
    /// The set this node serves.
    pub set_id: String,
    /// Name, unique ignoring case (`postgres/nodes/<name>` for local nodes).
    pub name: String,
    /// Where it runs.
    pub kind: NodeKind,
    /// Primary or replica.
    pub role: NodeRole,
    /// Connection URL sealed with the instance keyring (`None` for local
    /// nodes). Ciphertext only; the admin API must not return it.
    pub url_sealed: Option<String>,
    /// Health and lifecycle.
    pub state: NodeState,
    /// Share of replica reads (0: out of rotation).
    pub weight: u32,
    /// PostgreSQL `server_version`, when known.
    pub engine_version: Option<String>,
    /// `pg_control_system().system_identifier`, when known.
    pub system_identifier: Option<String>,
    /// Timeline id, when known.
    pub timeline: Option<i64>,
    /// API or `forgebase.toml`.
    pub origin: NodeOrigin,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Where one tenant lives (`_forgebase_tenant_placements`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// The tenant (environment id).
    pub tenant: String,
    /// The set holding its rows.
    pub set_id: String,
    /// Grows by one on every change of this row (≥ 1).
    pub epoch: u64,
    /// Active, moving, frozen or cleanup.
    pub state: PlacementState,
    /// RFC 3339 time of the last move's flip.
    pub moved_at: Option<String>,
    /// Estimated rows (statistics job).
    pub est_rows: u64,
    /// Estimated bytes (statistics job); the load score sums these.
    pub est_bytes: u64,
    /// RFC 3339 time of the estimate.
    pub stats_at: Option<String>,
}

/// Sets and nodes at one topology epoch: what `topology.json` holds.
/// Placements are not part of it (there can be very many; they stay in the
/// control table and the router's cache).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topology {
    /// The topology epoch (0 until the first change).
    pub epoch: u64,
    /// Every set, by number.
    pub sets: Vec<DbSet>,
    /// Every node, by set number then name.
    pub nodes: Vec<DbNode>,
}

impl Topology {
    /// The control set alone, no nodes, epoch 0: a single-server deployment.
    pub fn single() -> Self {
        Self {
            epoch: 0,
            sets: vec![DbSet::control()],
            nodes: Vec::new(),
        }
    }

    /// A set by id.
    pub fn set(&self, id: &str) -> Option<&DbSet> {
        self.sets.iter().find(|s| s.id == id)
    }

    /// The control set (a synthetic one if the rows lack it).
    pub fn control(&self) -> DbSet {
        self.sets
            .iter()
            .find(|s| s.is_control())
            .cloned()
            .unwrap_or_else(DbSet::control)
    }

    /// The current primary of a set (not retired), if it has a row.
    pub fn primary_of(&self, set_id: &str) -> Option<&DbNode> {
        self.nodes
            .iter()
            .find(|n| n.set_id == set_id && n.role == NodeRole::Primary && n.state != NodeState::Retired)
    }

    /// The nodes of a set.
    pub fn nodes_of<'a>(&'a self, set_id: &'a str) -> impl Iterator<Item = &'a DbNode> + 'a {
        self.nodes.iter().filter(move |n| n.set_id == set_id)
    }

    /// Whether more than one set can be routed to (sharding in use).
    pub fn is_sharded(&self) -> bool {
        self.sets.iter().filter(|s| s.is_routable()).count() > 1
    }
}

/// The identity values set *k* allocates: `[k·2⁴⁰ + 1, (k+1)·2⁴⁰ − 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceRange {
    /// Smallest value (also the start).
    pub min: i64,
    /// Largest value.
    pub max: i64,
}

impl SequenceRange {
    /// Whether `value` is in the range.
    pub fn contains(&self, value: i64) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

/// The identity range of set number `number` (ADR-0029 §3.3). Control
/// (0) is `[1, 2⁴⁰ − 1]`, so every id written before sharding is in it.
///
/// # Panics
/// When `number` is above [`MAX_SET_NUMBER`].
pub fn sequence_range(number: u16) -> SequenceRange {
    assert!(
        number <= MAX_SET_NUMBER,
        "set number {number} is above {MAX_SET_NUMBER}"
    );
    let base = i64::from(number) << RANGE_BITS;
    SequenceRange {
        min: base + 1,
        max: base + (1_i64 << RANGE_BITS) - 1,
    }
}

/// The set number an identity value (or realtime cursor) was allocated on:
/// `value >> 40` (ADR-0029 §3.3). Values ≤ 0 belong to control.
pub fn set_number_of(value: i64) -> u16 {
    if value <= 0 {
        return 0;
    }
    u16::try_from(value >> RANGE_BITS).unwrap_or(u16::MAX)
}

/// Confines every sequence in [`SEQUENCE_SCHEMAS`] (identity columns
/// included) to the range of set `number`, in one transaction, and returns
/// the names it changed (`schema.sequence`). Idempotent: sequences already
/// confined are left alone. A sequence whose position is inside the range
/// keeps it; one outside (a new shard's fresh sequences) restarts at the
/// range's start.
///
/// Run on every new shard before it becomes active (FB-174), and once on
/// control (number 0: only the maximum changes, ids continue as before).
/// SQLite has one set: number 0 is a no-op there, any other number is
/// [`DbError::Unsupported`].
pub async fn apply_sequence_range(db: &Database, number: u16) -> Result<Vec<String>, DbError> {
    if number > MAX_SET_NUMBER {
        return Err(DbError::Statement(format!(
            "set number {number} is above the maximum {MAX_SET_NUMBER}"
        )));
    }
    if db.kind() == BackendKind::Sqlite {
        return if number == 0 {
            Ok(Vec::new())
        } else {
            Err(DbError::unsupported("shards", BackendKind::Sqlite))
        };
    }
    let range = sequence_range(number);
    let schemas = SEQUENCE_SCHEMAS
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = db
        .fetch_all(&Statement::raw(format!(
            "SELECT schemaname::text, sequencename::text, min_value, max_value, start_value, last_value \
             FROM pg_catalog.pg_sequences WHERE schemaname IN ({schemas}) ORDER BY 1, 2"
        )))
        .await?;
    let mut changes = Vec::new();
    for row in &rows.rows {
        let text = |i: usize| row.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
        let int = |i: usize| row.get(i).and_then(Value::as_i64);
        let (schema, name) = (text(0), text(1));
        let (min, max, start, last) = (int(2), int(3), int(4), int(5));
        if min == Some(range.min) && max == Some(range.max) && start == Some(range.min) {
            continue;
        }
        let ident = format!(
            "{}.{}",
            db.dialect().quote_ident(&schema),
            db.dialect().quote_ident(&name)
        );
        let restart = match last {
            Some(v) if range.contains(v) => String::new(),
            _ => format!(" RESTART WITH {}", range.min),
        };
        changes.push((
            format!("{schema}.{name}"),
            format!(
                "ALTER SEQUENCE {ident} MINVALUE {} MAXVALUE {} START WITH {}{restart}",
                range.min, range.max, range.min
            ),
        ));
    }
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let mut tx = db.begin(TxMode::Normal).await?;
    for (_, sql) in &changes {
        tx.execute(&Statement::raw(sql.clone())).await?;
    }
    tx.commit().await?;
    Ok(changes.into_iter().map(|(n, _)| n).collect())
}

/// The `topology.json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyFile {
    /// [`TOPOLOGY_FILE_VERSION`].
    pub version: u32,
    /// RFC 3339 time it was written.
    pub written_at: String,
    /// Sets, nodes and epoch.
    #[serde(flatten)]
    pub topology: Topology,
}

/// Writes `topology` to `path` atomically (a temporary file renamed over
/// it), readable by the owner only on Unix.
pub fn write_topology_file(path: &Path, topology: &Topology) -> Result<(), DbError> {
    let doc = TopologyFile {
        version: TOPOLOGY_FILE_VERSION,
        written_at: forgebase_core::now_rfc3339(),
        topology: topology.clone(),
    };
    let json = serde_json::to_vec_pretty(&doc).map_err(|e| DbError::Statement(format!("topology.json: {e}")))?;
    let io = |source: std::io::Error| DbError::Io {
        path: path.to_path_buf(),
        source,
    };
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp).map_err(io)?;
        file.write_all(&json).map_err(io)?;
        file.write_all(b"\n").map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(io)
}

/// Reads `topology.json`; `None` when the file does not exist.
pub fn read_topology_file(path: &Path) -> Result<Option<TopologyFile>, DbError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DbError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let doc: TopologyFile = serde_json::from_slice(&bytes)
        .map_err(|e| DbError::Statement(format!("{} is not a topology file: {e}", path.display())))?;
    if doc.version != TOPOLOGY_FILE_VERSION {
        return Err(DbError::Unsupported(format!(
            "{} has format version {}, this binary reads {TOPOLOGY_FILE_VERSION}",
            path.display(),
            doc.version
        )));
    }
    Ok(Some(doc))
}

// ---------------------------------------------------------------------------
// Store: reads and writes of the control tables.
// ---------------------------------------------------------------------------

const SET_COLUMNS: &str = "id, name, kind, number, state, weight, hosts_tenants, created_at, updated_at";
const NODE_COLUMNS: &str = "id, set_id, name, kind, role, url_sealed, state, weight, engine_version, \
                            system_identifier, timeline, origin, created_at, updated_at";
const PLACEMENT_COLUMNS: &str = "tenant, set_id, epoch, state, moved_at, est_rows, est_bytes, stats_at";

/// Reads sets, nodes and the topology epoch from control.
pub async fn load(control: &dyn Backend) -> Result<Topology, DbError> {
    let sets = control
        .fetch_all(&Statement::new(
            format!("SELECT {SET_COLUMNS} FROM _forgebase_db_sets ORDER BY number"),
            vec![],
        ))
        .await?;
    let nodes = control
        .fetch_all(&Statement::new(
            format!(
                "SELECT n.{} FROM _forgebase_db_nodes n JOIN _forgebase_db_sets s ON s.id = n.set_id \
                 ORDER BY s.number, n.name",
                NODE_COLUMNS.replace(", ", ", n.")
            ),
            vec![],
        ))
        .await?;
    let epoch = control
        .fetch_all(&Statement::new(
            "SELECT value FROM _forgebase_meta WHERE key = ?1",
            vec![EPOCH_META_KEY.into()],
        ))
        .await?;
    Ok(Topology {
        epoch: epoch
            .scalar()
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        sets: sets.rows.iter().map(|r| set_from_row(r)).collect::<Result<_, _>>()?,
        nodes: nodes.rows.iter().map(|r| node_from_row(r)).collect::<Result<_, _>>()?,
    })
}

/// Reads every placement row from control.
pub async fn load_placements(control: &dyn Backend) -> Result<Vec<Placement>, DbError> {
    let rows = control
        .fetch_all(&Statement::new(
            format!("SELECT {PLACEMENT_COLUMNS} FROM _forgebase_tenant_placements ORDER BY tenant"),
            vec![],
        ))
        .await?;
    rows.rows.iter().map(|r| placement_from_row(r)).collect()
}

/// Reads one tenant's placement from control.
pub async fn load_placement(control: &dyn Backend, tenant: &str) -> Result<Option<Placement>, DbError> {
    let rows = control
        .fetch_all(&Statement::new(
            format!("SELECT {PLACEMENT_COLUMNS} FROM _forgebase_tenant_placements WHERE tenant = ?1"),
            vec![tenant.into()],
        ))
        .await?;
    rows.rows.first().map(|r| placement_from_row(r)).transpose()
}

/// Increments the topology epoch inside `tx` and returns the new value.
pub async fn bump_epoch(tx: &mut dyn WriteTx) -> Result<u64, DbError> {
    let rows = tx
        .fetch_all(&Statement::new(
            "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, '1', ?2) \
             ON CONFLICT (key) DO UPDATE SET \
             value = CAST(CAST(_forgebase_meta.value AS BIGINT) + 1 AS TEXT), updated_at = excluded.updated_at \
             RETURNING value",
            vec![EPOCH_META_KEY.into(), forgebase_core::now_rfc3339().into()],
        ))
        .await?;
    epoch_of(&rows)
}

fn epoch_of(rows: &Rows) -> Result<u64, DbError> {
    rows.scalar()
        .and_then(Value::as_str)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| DbError::Statement("the topology epoch is not a number".into()))
}

/// A shard to add with [`insert_set`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSet {
    /// Display name (unique ignoring case).
    pub name: String,
    /// Relative capacity (≥ 1).
    pub weight: u32,
}

/// Adds a shard in state `provisioning` with the next unused number (numbers
/// are never reused: rows allocated from a retired set's range may still
/// exist elsewhere) and bumps the epoch. PostgreSQL only.
pub async fn insert_set(control: &Database, new: &NewSet) -> Result<DbSet, DbError> {
    if control.kind() == BackendKind::Sqlite {
        return Err(DbError::unsupported("shards", BackendKind::Sqlite));
    }
    let mut tx = control.begin(TxMode::Normal).await?;
    let next = tx
        .fetch_all(&Statement::raw(
            "SELECT coalesce(max(number), 0) + 1 FROM _forgebase_db_sets",
        ))
        .await?
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let number = u16::try_from(next)
        .ok()
        .filter(|n| *n <= MAX_SET_NUMBER)
        .ok_or_else(|| DbError::Statement(format!("no set number left (maximum {MAX_SET_NUMBER})")))?;
    let now = forgebase_core::now_rfc3339();
    let id = format!("s{}", &uuid::Uuid::new_v4().simple().to_string()[..14]);
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "INSERT INTO _forgebase_db_sets (id, name, kind, number, state, weight, hosts_tenants, created_at, updated_at) \
                 VALUES (?1, ?2, 'shard', ?3, 'provisioning', ?4, 1, ?5, ?5) RETURNING {SET_COLUMNS}"
            ),
            vec![
                id.into(),
                new.name.clone().into(),
                i64::from(number).into(),
                i64::from(new.weight).into(),
                now.into(),
            ],
        ))
        .await?;
    let set = rows
        .rows
        .first()
        .map(|r| set_from_row(r))
        .transpose()?
        .ok_or_else(|| DbError::Statement("insert into _forgebase_db_sets returned nothing".into()))?;
    bump_epoch(tx.as_mut()).await?;
    tx.commit().await?;
    Ok(set)
}

/// Changes a set's state, weight or `hosts_tenants` (each `None` keeps its
/// value) and bumps the epoch. Returns the updated set, `None` if unknown.
pub async fn update_set(
    control: &dyn Backend,
    id: &str,
    state: Option<SetState>,
    weight: Option<u32>,
    hosts_tenants: Option<bool>,
) -> Result<Option<DbSet>, DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "UPDATE _forgebase_db_sets SET state = coalesce(?2, state), weight = coalesce(?3, weight), \
                 hosts_tenants = coalesce(?4, hosts_tenants), updated_at = ?5 WHERE id = ?1 RETURNING {SET_COLUMNS}"
            ),
            vec![
                id.into(),
                state.map(SetState::as_str).into(),
                weight.map(i64::from).into(),
                hosts_tenants.into(),
                forgebase_core::now_rfc3339().into(),
            ],
        ))
        .await?;
    let Some(set) = rows.rows.first().map(|r| set_from_row(r)).transpose()? else {
        return Ok(None);
    };
    bump_epoch(tx.as_mut()).await?;
    tx.commit().await?;
    Ok(Some(set))
}

/// A node to add with [`insert_node`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewNode {
    /// The set it serves.
    pub set_id: String,
    /// Name (unique ignoring case).
    pub name: String,
    /// Where it runs.
    pub kind: NodeKind,
    /// Primary or replica.
    pub role: NodeRole,
    /// Sealed connection URL (`None` for local nodes).
    pub url_sealed: Option<String>,
    /// Initial state.
    pub state: NodeState,
    /// Share of replica reads.
    pub weight: u32,
    /// API or `forgebase.toml`.
    pub origin: NodeOrigin,
}

/// Adds a node and bumps the epoch.
pub async fn insert_node(control: &dyn Backend, new: &NewNode) -> Result<DbNode, DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let now = forgebase_core::now_rfc3339();
    let id = format!("n{}", &uuid::Uuid::new_v4().simple().to_string()[..14]);
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "INSERT INTO _forgebase_db_nodes (id, set_id, name, kind, role, url_sealed, state, weight, origin, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10) RETURNING {NODE_COLUMNS}"
            ),
            vec![
                id.into(),
                new.set_id.clone().into(),
                new.name.clone().into(),
                new.kind.as_str().into(),
                new.role.as_str().into(),
                new.url_sealed.clone().into(),
                new.state.as_str().into(),
                i64::from(new.weight).into(),
                new.origin.as_str().into(),
                now.into(),
            ],
        ))
        .await?;
    let node = rows
        .rows
        .first()
        .map(|r| node_from_row(r))
        .transpose()?
        .ok_or_else(|| DbError::Statement("insert into _forgebase_db_nodes returned nothing".into()))?;
    bump_epoch(tx.as_mut()).await?;
    tx.commit().await?;
    Ok(node)
}

/// Changes a node's state (health monitor, promote, rebuild) and bumps the
/// epoch. Returns the updated node, `None` if unknown.
pub async fn update_node_state(control: &dyn Backend, id: &str, state: NodeState) -> Result<Option<DbNode>, DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "UPDATE _forgebase_db_nodes SET state = ?2, updated_at = ?3 WHERE id = ?1 RETURNING {NODE_COLUMNS}"
            ),
            vec![id.into(), state.as_str().into(), forgebase_core::now_rfc3339().into()],
        ))
        .await?;
    let Some(node) = rows.rows.first().map(|r| node_from_row(r)).transpose()? else {
        return Ok(None);
    };
    bump_epoch(tx.as_mut()).await?;
    tx.commit().await?;
    Ok(Some(node))
}

/// Writes a tenant's placement inside `tx`: inserts it (epoch 1) or, when
/// the set or state differs, updates it with `epoch + 1` (and `moved_at`
/// when the set changes). Returns the row and whether it changed. Does not
/// bump the topology epoch: the caller does, once per transaction.
pub async fn upsert_placement(
    tx: &mut dyn WriteTx,
    tenant: &str,
    set_id: &str,
    state: PlacementState,
) -> Result<(Placement, bool), DbError> {
    let now = forgebase_core::now_rfc3339();
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "INSERT INTO _forgebase_tenant_placements (tenant, set_id, epoch, state, created_at, updated_at) \
                 VALUES (?1, ?2, 1, ?3, ?4, ?4) \
                 ON CONFLICT (tenant) DO UPDATE SET \
                 moved_at = CASE WHEN _forgebase_tenant_placements.set_id <> excluded.set_id \
                     THEN excluded.updated_at ELSE _forgebase_tenant_placements.moved_at END, \
                 set_id = excluded.set_id, state = excluded.state, \
                 epoch = _forgebase_tenant_placements.epoch + 1, updated_at = excluded.updated_at \
                 WHERE _forgebase_tenant_placements.set_id <> excluded.set_id \
                    OR _forgebase_tenant_placements.state <> excluded.state \
                 RETURNING {PLACEMENT_COLUMNS}"
            ),
            vec![tenant.into(), set_id.into(), state.as_str().into(), now.into()],
        ))
        .await?;
    if let Some(row) = rows.rows.first() {
        return Ok((placement_from_row(row)?, true));
    }
    // Unchanged: the conflict update's WHERE matched nothing.
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {PLACEMENT_COLUMNS} FROM _forgebase_tenant_placements WHERE tenant = ?1"),
            vec![tenant.into()],
        ))
        .await?;
    let row = rows
        .rows
        .first()
        .ok_or_else(|| DbError::Statement(format!("placement of {tenant} vanished")))?;
    Ok((placement_from_row(row)?, false))
}

/// Records size estimates for a tenant's placement (the statistics job).
/// Not a topology change: the epochs stay.
pub async fn record_placement_stats(
    control: &dyn Backend,
    tenant: &str,
    est_rows: u64,
    est_bytes: u64,
) -> Result<bool, DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let r = tx
        .execute(&Statement::new(
            "UPDATE _forgebase_tenant_placements SET est_rows = ?2, est_bytes = ?3, stats_at = ?4 WHERE tenant = ?1",
            vec![
                tenant.into(),
                i64::try_from(est_rows).unwrap_or(i64::MAX).into(),
                i64::try_from(est_bytes).unwrap_or(i64::MAX).into(),
                forgebase_core::now_rfc3339().into(),
            ],
        ))
        .await?;
    tx.commit().await?;
    Ok(r.rows_affected > 0)
}

// ---------------------------------------------------------------------------
// Row decoding.
// ---------------------------------------------------------------------------

fn text_at(row: &[Value], i: usize, what: &str) -> Result<String, DbError> {
    row.get(i)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DbError::Statement(format!("topology column {what} is not text")))
}

fn opt_text_at(row: &[Value], i: usize) -> Option<String> {
    row.get(i).and_then(Value::as_str).map(str::to_owned)
}

fn int_at(row: &[Value], i: usize, what: &str) -> Result<i64, DbError> {
    row.get(i)
        .and_then(Value::as_i64)
        .ok_or_else(|| DbError::Statement(format!("topology column {what} is not an integer")))
}

fn unsigned<T: TryFrom<i64>>(v: i64, what: &str) -> Result<T, DbError> {
    T::try_from(v).map_err(|_| DbError::Statement(format!("topology column {what} is out of range: {v}")))
}

fn set_from_row(row: &[Value]) -> Result<DbSet, DbError> {
    Ok(DbSet {
        id: text_at(row, 0, "id")?,
        name: text_at(row, 1, "name")?,
        kind: text_at(row, 2, "kind")?.parse()?,
        number: unsigned(int_at(row, 3, "number")?, "number")?,
        state: text_at(row, 4, "state")?.parse()?,
        weight: unsigned(int_at(row, 5, "weight")?, "weight")?,
        hosts_tenants: int_at(row, 6, "hosts_tenants")? != 0,
        created_at: text_at(row, 7, "created_at")?,
        updated_at: text_at(row, 8, "updated_at")?,
    })
}

fn node_from_row(row: &[Value]) -> Result<DbNode, DbError> {
    Ok(DbNode {
        id: text_at(row, 0, "id")?,
        set_id: text_at(row, 1, "set_id")?,
        name: text_at(row, 2, "name")?,
        kind: text_at(row, 3, "kind")?.parse()?,
        role: text_at(row, 4, "role")?.parse()?,
        url_sealed: opt_text_at(row, 5),
        state: text_at(row, 6, "state")?.parse()?,
        weight: unsigned(int_at(row, 7, "weight")?, "weight")?,
        engine_version: opt_text_at(row, 8),
        system_identifier: opt_text_at(row, 9),
        timeline: row.get(10).and_then(Value::as_i64),
        origin: text_at(row, 11, "origin")?.parse()?,
        created_at: text_at(row, 12, "created_at")?,
        updated_at: text_at(row, 13, "updated_at")?,
    })
}

fn placement_from_row(row: &[Value]) -> Result<Placement, DbError> {
    Ok(Placement {
        tenant: text_at(row, 0, "tenant")?,
        set_id: text_at(row, 1, "set_id")?,
        epoch: unsigned(int_at(row, 2, "epoch")?, "epoch")?,
        state: text_at(row, 3, "state")?.parse()?,
        moved_at: opt_text_at(row, 4),
        est_rows: unsigned(int_at(row, 5, "est_rows")?, "est_rows")?,
        est_bytes: unsigned(int_at(row, 6, "est_bytes")?, "est_bytes")?,
        stats_at: opt_text_at(row, 7),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_disjoint_and_below_2_pow_53() {
        assert_eq!(
            sequence_range(0),
            SequenceRange {
                min: 1,
                max: (1 << 40) - 1
            }
        );
        assert_eq!(sequence_range(1).min, (1 << 40) + 1);
        assert_eq!(sequence_range(0).max + 2, sequence_range(1).min);
        let last = sequence_range(MAX_SET_NUMBER);
        assert!(last.max < (1_i64 << 53), "ids stay exact in JavaScript");
        for k in [0, 1, 7, MAX_SET_NUMBER] {
            let r = sequence_range(k);
            assert_eq!(set_number_of(r.min), k);
            assert_eq!(set_number_of(r.max), k);
        }
        assert_eq!(set_number_of(0), 0);
        assert_eq!(set_number_of(-5), 0);
    }

    #[test]
    #[should_panic(expected = "above")]
    fn ranges_stop_at_the_maximum() {
        let _ = sequence_range(MAX_SET_NUMBER + 1);
    }

    #[test]
    fn enums_round_trip_their_stored_text() {
        fn check<T>(all: &[T])
        where
            T: Copy + fmt::Display + FromStr<Err = DbError> + PartialEq + fmt::Debug + Serialize,
        {
            for v in all {
                let text = v.to_string();
                assert_eq!(text.parse::<T>().unwrap(), *v);
                assert_eq!(serde_json::to_value(v).unwrap(), serde_json::Value::String(text));
            }
        }
        check(SetKind::ALL);
        check(SetState::ALL);
        check(NodeKind::ALL);
        check(NodeRole::ALL);
        check(NodeState::ALL);
        check(NodeOrigin::ALL);
        check(PlacementState::ALL);
        check(MoveState::ALL);
        check(XactState::ALL);
        assert!("nope".parse::<SetState>().is_err());
    }

    #[test]
    fn enum_texts_match_the_migration_checks() {
        let sqlite = include_str!("../../../migrations/system/0033_db_topology.sql");
        let pg = include_str!("../../../migrations/postgres/0033_db_topology.sql");
        let texts = [
            SetKind::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
            SetState::ALL.iter().map(|v| v.as_str()).collect(),
            NodeKind::ALL.iter().map(|v| v.as_str()).collect(),
            NodeRole::ALL.iter().map(|v| v.as_str()).collect(),
            NodeState::ALL.iter().map(|v| v.as_str()).collect(),
            NodeOrigin::ALL.iter().map(|v| v.as_str()).collect(),
            PlacementState::ALL.iter().map(|v| v.as_str()).collect(),
            MoveState::ALL.iter().map(|v| v.as_str()).collect(),
            XactState::ALL.iter().map(|v| v.as_str()).collect(),
        ];
        for list in texts {
            let check = format!(
                "IN ({})",
                list.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(", ")
            );
            let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(squash(sqlite).contains(&check), "SQLite migration lacks {check}");
            assert!(squash(pg).contains(&check), "PostgreSQL migration lacks {check}");
        }
    }

    #[test]
    fn single_topology_is_control_only() {
        let t = Topology::single();
        assert!(!t.is_sharded());
        assert_eq!(t.control().id, CONTROL_SET_ID);
        assert!(t.control().takes_new_tenants());
        assert!(t.primary_of(CONTROL_SET_ID).is_none());
    }

    #[test]
    fn topology_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TOPOLOGY_FILE);
        assert!(read_topology_file(&path).unwrap().is_none());
        let mut t = Topology::single();
        t.epoch = 7;
        t.nodes.push(DbNode {
            id: "n1".into(),
            set_id: CONTROL_SET_ID.into(),
            name: "primary".into(),
            kind: NodeKind::External,
            role: NodeRole::Primary,
            url_sealed: Some("sealed".into()),
            state: NodeState::Ready,
            weight: 1,
            engine_version: Some("18.6".into()),
            system_identifier: None,
            timeline: Some(1),
            origin: NodeOrigin::Config,
            created_at: "x".into(),
            updated_at: "x".into(),
        });
        write_topology_file(&path, &t).unwrap();
        let back = read_topology_file(&path).unwrap().unwrap();
        assert_eq!(back.version, TOPOLOGY_FILE_VERSION);
        assert_eq!(back.topology, t);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(
            &path,
            br#"{"version": 9, "written_at": "", "epoch": 0, "sets": [], "nodes": []}"#,
        )
        .unwrap();
        assert!(matches!(read_topology_file(&path), Err(DbError::Unsupported(_))));
    }
}
