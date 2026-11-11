//! Read-your-writes headers (ADR-0029 §2.6, FB-167).
//!
//! [`track`] runs every request inside a
//! [`RequestScope`](forgebase_db::routing::RequestScope):
//!
//! * `X-ForgeBase-Consistency: strong` sends the request's routed reads to
//!   the primary;
//! * `X-ForgeBase-Read-After: <set>/<lsn>[, …]` (the SDKs send the highest
//!   commit token they saw per set) holds replica reads back until a
//!   replica has replayed those positions, else they read the primary;
//! * for clients without an SDK (the dashboard, curl) the server also
//!   remembers each principal's latest commit per set for 10 seconds
//!   ([`RecentCommits`], at most 10 000 principals) and applies it to that
//!   principal's later requests; anonymous callers are not remembered;
//! * a response to a request that committed on a set with replicas carries
//!   `X-ForgeBase-Commit: <set>/<lsn>[, …]`.
//!
//! Without replicas no commit records a token, so responses are unchanged.
//! Malformed header values are ignored (they only ever make reads fresher).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use forgebase_data::{AccessContext, Principal};
use forgebase_db::routing::scope::{self, RequestScope};
use forgebase_db::{Consistency, ReadAfter};

/// Response header: the commit positions of the request's writes.
pub const COMMIT_HEADER: &str = "x-forgebase-commit";
/// Request header: commit positions the request's reads must see.
pub const READ_AFTER_HEADER: &str = "x-forgebase-read-after";
/// Request header: `strong` reads the primary.
pub const CONSISTENCY_HEADER: &str = "x-forgebase-consistency";
/// How long a principal's last commit is remembered.
pub const RECENT_TTL: Duration = Duration::from_secs(10);
/// Principals remembered at most (least recently written dropped first).
pub const RECENT_CAPACITY: usize = 10_000;

/// The latest commit per set of recent writers, by principal.
#[derive(Debug, Clone)]
pub struct RecentCommits {
    inner: Arc<Mutex<Recent>>,
    ttl: Duration,
    capacity: usize,
}

#[derive(Debug, Default)]
struct Recent {
    by_principal: HashMap<String, (ReadAfter, Instant)>,
    /// Write order, oldest first; entries superseded by a later write of
    /// the same principal are skipped when they reach the front.
    order: VecDeque<(String, Instant)>,
}

impl Default for RecentCommits {
    fn default() -> Self {
        Self::new(RECENT_TTL, RECENT_CAPACITY)
    }
}

impl RecentCommits {
    /// A map that keeps commits for `ttl`, for at most `capacity` principals.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Recent::default())),
            ttl,
            capacity: capacity.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Recent> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The principal's commits from the last `ttl`.
    pub fn get(&self, principal: &str) -> ReadAfter {
        let recent = self.lock();
        match recent.by_principal.get(principal) {
            Some((commits, at)) if at.elapsed() <= self.ttl => commits.clone(),
            _ => ReadAfter::new(),
        }
    }

    /// Remembers `commits` of `principal` (merged with its fresh ones).
    pub fn note(&self, principal: &str, commits: &ReadAfter) {
        let now = Instant::now();
        let mut recent = self.lock();
        let entry = recent
            .by_principal
            .entry(principal.to_owned())
            .or_insert_with(|| (ReadAfter::new(), now));
        if entry.1.elapsed() > self.ttl {
            entry.0 = ReadAfter::new();
        }
        entry.0.merge(commits);
        entry.1 = now;
        recent.order.push_back((principal.to_owned(), now));
        // Drop expired and least recently written principals.
        while let Some((key, at)) = recent.order.front().cloned() {
            let over = recent.by_principal.len() > self.capacity || recent.order.len() > 2 * self.capacity;
            let expired = at.elapsed() > self.ttl;
            if !over && !expired {
                break;
            }
            recent.order.pop_front();
            if recent.by_principal.get(&key).is_some_and(|(_, last)| *last == at) {
                recent.by_principal.remove(&key);
            }
        }
    }

    /// Principals remembered.
    pub fn len(&self) -> usize {
        self.lock().by_principal.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Key of an identified caller (realm, kind, id); `None` for anonymous and
/// system calls.
fn principal_key(ctx: &AccessContext) -> Option<String> {
    let who = match &ctx.principal {
        Principal::Admin { id } => format!("admin:{id}"),
        Principal::User { collection, id } => format!("user:{collection}:{id}"),
        Principal::Service { id, .. } => format!("service:{id}"),
        Principal::Anonymous | Principal::System => return None,
    };
    Some(format!("{}/{who}", ctx.realm_id))
}

/// The consistency layer: mounted inside authentication, so the
/// request's [`AccessContext`] is known.
pub async fn track(State(recent): State<RecentCommits>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let consistency = headers
        .get(CONSISTENCY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(Consistency::parse)
        .unwrap_or_default();
    let mut read_after = ReadAfter::new();
    for value in headers.get_all(READ_AFTER_HEADER) {
        if let Ok(v) = value.to_str() {
            read_after.parse_into(v);
        }
    }
    let principal = req.extensions().get::<AccessContext>().and_then(principal_key);
    if let Some(p) = &principal {
        read_after.merge(&recent.get(p));
    }
    let scope = RequestScope::new(consistency, read_after);
    let mut res = scope::scoped(Arc::clone(&scope), next.run(req)).await;
    let commits = scope.commits();
    if let Some(value) = commits.to_header() {
        if let Some(p) = &principal {
            recent.note(p, &commits);
        }
        if let Ok(v) = HeaderValue::from_str(&value) {
            res.headers_mut().insert(COMMIT_HEADER, v);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::middleware;
    use axum::routing::{get, post};
    use forgebase_db::Lsn;
    use tower::ServiceExt;

    use super::*;

    /// Test auth: `x-test-user: <id>` is an administrator, else anonymous.
    async fn fake_auth(mut req: Request, next: Next) -> Response {
        let ctx = match req.headers().get("x-test-user").and_then(|v| v.to_str().ok()) {
            Some(id) => AccessContext::admin(id),
            None => AccessContext::anonymous(),
        };
        req.extensions_mut().insert(ctx);
        next.run(req).await
    }

    async fn write() -> &'static str {
        scope::record_commit("control", Lsn(0x10));
        scope::record_commit("s1", Lsn(0x1_0000_0002));
        "ok"
    }

    /// Answers what the request's reads would need.
    async fn read() -> String {
        let s = scope::current().expect("a request scope");
        format!(
            "{}|{}",
            s.consistency().as_str(),
            s.read_after().to_header().unwrap_or_default()
        )
    }

    fn app(recent: RecentCommits) -> Router {
        Router::new()
            .route("/w", post(write))
            .route("/r", get(read))
            .layer(middleware::from_fn_with_state(recent, track))
            .layer(middleware::from_fn(fake_auth))
    }

    async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> (Option<String>, String) {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let commit = res.headers().get(COMMIT_HEADER).map(|v| v.to_str().unwrap().to_owned());
        let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
        (commit, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn commits_are_answered_and_remembered_per_principal() {
        let recent = RecentCommits::default();
        let app = app(recent.clone());
        let (commit, _) = call(&app, "GET", "/r", &[("x-test-user", "a")]).await;
        assert_eq!(commit, None, "no commit, no header");
        let (commit, _) = call(&app, "POST", "/w", &[("x-test-user", "a")]).await;
        assert_eq!(commit.as_deref(), Some("control/0/10, s1/1/2"));
        // The same principal's next read waits for its writes; others do not.
        let (_, body) = call(&app, "GET", "/r", &[("x-test-user", "a")]).await;
        assert_eq!(body, "session|control/0/10, s1/1/2");
        let (_, body) = call(&app, "GET", "/r", &[("x-test-user", "b")]).await;
        assert_eq!(body, "session|");
        // Anonymous writers are answered but not remembered.
        let (commit, _) = call(&app, "POST", "/w", &[]).await;
        assert!(commit.is_some());
        assert_eq!(recent.len(), 1);
        let (_, body) = call(&app, "GET", "/r", &[]).await;
        assert_eq!(body, "session|");
    }

    #[tokio::test]
    async fn read_after_and_consistency_headers_are_parsed() {
        let app = app(RecentCommits::default());
        let (_, body) = call(
            &app,
            "GET",
            "/r",
            &[
                (READ_AFTER_HEADER, "control/0/5, junk"),
                (READ_AFTER_HEADER, "control/0/9"),
                (CONSISTENCY_HEADER, "Strong"),
            ],
        )
        .await;
        assert_eq!(body, "strong|control/0/9");
        let (_, body) = call(&app, "GET", "/r", &[(CONSISTENCY_HEADER, "whatever")]).await;
        assert_eq!(body, "session|");
        // A principal's remembered commits merge with the header (higher wins).
        let recent = RecentCommits::default();
        recent.note(
            &format!("{}/admin:a", forgebase_data::DEFAULT_REALM),
            &ReadAfter::parse_header("control/0/7, s2/0/1"),
        );
        let app = self::app(recent);
        let (_, body) = call(
            &app,
            "GET",
            "/r",
            &[("x-test-user", "a"), (READ_AFTER_HEADER, "control/0/9")],
        )
        .await;
        assert_eq!(body, "session|control/0/9, s2/0/1");
    }

    /// The whole path over a real standby: a write answers its token, a
    /// read sent with it waits for the replica and then reads the primary;
    /// the dashboard-style read without a token gets the same through the
    /// principal map.
    #[cfg(unix)]
    #[tokio::test]
    async fn tokens_hold_replica_reads_back_over_http() {
        use axum::extract::State as S;
        use forgebase_db::routing::testing::ReplicaCluster;
        use forgebase_db::routing::{ReplicaNode, ReplicaOptions, ReplicaSet};
        use forgebase_db::topology::{DbNode, DbSet, NodeKind, NodeOrigin, NodeRole, NodeState};
        use forgebase_db::{Backend, ReadIntent, SetHandle, Statement, TxMode};

        let Some(cluster) = ReplicaCluster::start(&[]).await else {
            eprintln!("skipped: no PostgreSQL engine configured");
            return;
        };
        let cluster = cluster.unwrap();
        let primary = cluster.open_primary().await.unwrap();
        let mut tx = primary.begin(TxMode::Normal).await.unwrap();
        tx.execute(&Statement::raw("CREATE TABLE rt (id bigint PRIMARY KEY)"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        cluster.wait_caught_up().await.unwrap();
        let node = DbNode {
            id: "n1".into(),
            set_id: "control".into(),
            name: "r1".into(),
            kind: NodeKind::External,
            role: NodeRole::Replica,
            url_sealed: None,
            state: NodeState::Ready,
            weight: 1,
            engine_version: None,
            system_identifier: None,
            timeline: None,
            origin: NodeOrigin::Api,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let replicas = ReplicaSet::open(
            "control",
            primary.clone(),
            vec![ReplicaNode {
                node,
                db: cluster.open_standby().await.unwrap(),
            }],
            ReplicaOptions {
                health_interval: Duration::ZERO,
                ..ReplicaOptions::default()
            },
            None,
        )
        .await;
        let handle = SetHandle::new(DbSet::control(), primary).with_replicas(replicas);

        async fn insert(S(h): S<SetHandle>) -> &'static str {
            let mut tx = h.begin(TxMode::Normal).await.unwrap();
            tx.execute(&Statement::raw("INSERT INTO rt VALUES (1)")).await.unwrap();
            tx.commit().await.unwrap();
            "ok"
        }
        async fn select(S(h): S<SetHandle>) -> String {
            let sql = "SELECT CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END, (SELECT count(*) FROM rt)";
            let rows = h
                .fetch_routed(&Statement::new(sql, Vec::new()), &ReadIntent::replica("test"))
                .await
                .unwrap();
            let r = &rows.rows[0];
            format!("{},{}", r[0].as_i64().unwrap(), r[1].as_i64().unwrap())
        }
        let app = Router::new()
            .route("/w", post(insert))
            .route("/r", get(select))
            .with_state(handle)
            .layer(middleware::from_fn_with_state(RecentCommits::default(), track))
            .layer(middleware::from_fn(fake_auth));

        cluster.pause_replay().await.unwrap();
        let (token, _) = call(&app, "POST", "/w", &[("x-test-user", "a")]).await;
        let token = token.expect("X-ForgeBase-Commit");
        assert!(token.starts_with("control/"), "{token}");
        assert_eq!(
            call(&app, "GET", "/r", &[]).await.1,
            "1,0",
            "no token: the stale replica"
        );
        assert_eq!(call(&app, "GET", "/r", &[(READ_AFTER_HEADER, &token)]).await.1, "0,1");
        assert_eq!(call(&app, "GET", "/r", &[("x-test-user", "a")]).await.1, "0,1");
        assert_eq!(
            call(&app, "GET", "/r", &[(CONSISTENCY_HEADER, "strong")]).await.1,
            "0,1"
        );
        cluster.resume_replay().await.unwrap();
        assert_eq!(call(&app, "GET", "/r", &[(READ_AFTER_HEADER, &token)]).await.1, "1,1");
    }

    #[test]
    fn recent_commits_expire_and_are_bounded() {
        let recent = RecentCommits::new(Duration::from_millis(30), 2);
        let c = ReadAfter::parse_header("control/0/1");
        recent.note("a", &c);
        recent.note("b", &c);
        recent.note("a", &ReadAfter::parse_header("control/0/2"));
        assert_eq!(recent.get("a").get("control"), Some(Lsn(2)));
        recent.note("c", &c);
        assert_eq!(recent.len(), 2, "the least recently written principal goes");
        assert!(recent.get("b").is_empty());
        assert!(!recent.get("a").is_empty() && !recent.get("c").is_empty());
        std::thread::sleep(Duration::from_millis(40));
        assert!(recent.get("a").is_empty(), "expired");
        recent.note("d", &c);
        assert_eq!(recent.len(), 1, "expired principals are dropped on the next write");
        let key = principal_key(&AccessContext::user("users", "u1")).unwrap();
        assert_eq!(key, format!("{}/user:users:u1", forgebase_data::DEFAULT_REALM));
        assert_eq!(principal_key(&AccessContext::anonymous()), None);
        assert_eq!(principal_key(&AccessContext::system()), None);
    }
}
