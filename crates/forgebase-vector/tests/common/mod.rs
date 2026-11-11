//! Shared harness: a real, migrated SQLite database in a temp dir and a
//! vector service whose index files live in that data directory.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, Authorizer, DataService, DefaultAuthorizer};
use forgebase_db::Database;
use forgebase_db::testing::{TestDb, test_backend};
use forgebase_vector::model::*;
use forgebase_vector::{VectorConfig, VectorService};

pub struct Harness {
    pub db: Database,
    pub svc: VectorService,
    pub dir: tempfile::TempDir,
    pub test: TestDb,
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn tenant(t: &str) -> AccessContext {
    let mut ctx = AccessContext::admin("root");
    ctx.tenant = Some(t.to_owned());
    ctx
}

pub fn layout(dir: &Path) -> DataLayout {
    DataLayout::new(dir.join("data"))
}

pub fn index_dir(dir: &Path) -> PathBuf {
    layout(dir).indexes_dir().join("vector")
}

pub async fn test_db(dir: &Path) -> TestDb {
    layout(dir).ensure().unwrap();
    test_backend().await
}

pub async fn service(
    db: &Database,
    dir: &Path,
    config: VectorConfig,
    authorizer: Arc<dyn Authorizer>,
) -> VectorService {
    let data = DataService::new(Arc::new(db.clone()), authorizer).await.unwrap();
    VectorService::open(data, layout(dir).indexes_dir(), config)
        .await
        .unwrap()
}

pub async fn harness() -> Harness {
    harness_with(VectorConfig::default()).await
}

pub async fn harness_with(config: VectorConfig) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let test = test_db(dir.path()).await;
    let db = test.database().clone();
    let svc = service(&db, dir.path(), config, Arc::new(DefaultAuthorizer)).await;
    Harness { db, svc, dir, test }
}

impl Harness {
    /// Clean shutdown (indexes saved) and a fresh service on the same data
    /// directory, as after a restart.
    pub async fn restart(self, config: VectorConfig) -> Harness {
        self.svc.shutdown().await;
        self.reopen_without_shutdown(config).await
    }

    /// A fresh service without saving first, as after a crash.
    pub async fn reopen_without_shutdown(self, config: VectorConfig) -> Harness {
        let Harness { db, dir, test, .. } = self;
        db.close().await;
        let db = Database::open(test.options()).await.unwrap();
        let svc = service(&db, dir.path(), config, Arc::new(DefaultAuthorizer)).await;
        Harness { db, svc, dir, test }
    }
}

pub fn field(name: &str, dimension: u32, metric: Metric) -> FieldSpec {
    FieldSpec {
        name: name.into(),
        dimension,
        metric,
        quantization: None,
        connectivity: None,
        expansion_add: None,
        expansion_search: None,
    }
}

pub async fn create(svc: &VectorService, ctx: &AccessContext, name: &str, fields: Vec<FieldSpec>, exact: Option<u32>) {
    svc.create_collection(
        ctx,
        NewCollection {
            name: name.into(),
            description: String::new(),
            fields,
            exact_threshold: exact,
            source_collection: None,
        },
    )
    .await
    .unwrap();
}

pub fn item(id: &str, field: &str, v: &[f32], metadata: serde_json::Value) -> UpsertItem {
    UpsertItem {
        id: Some(id.into()),
        vectors: [(field.to_owned(), v.iter().map(|x| f64::from(*x)).collect())].into(),
        metadata: metadata.as_object().cloned(),
        text: None,
    }
}

pub fn query(field: &str, v: &[f32], k: u32) -> SearchRequest {
    SearchRequest {
        field: Some(field.into()),
        vector: v.iter().map(|x| f64::from(*x)).collect(),
        k,
        filter: None,
        params: Default::default(),
        filter_mode: FilterMode::Auto,
        exact: false,
        include_vectors: false,
        include_metadata: true,
        include_text: false,
    }
}

/// Deterministic pseudo-random numbers (xorshift64*), no extra dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1, 1).
    pub fn unit(&mut self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let x = (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32;
        x * 2.0 - 1.0
    }

    pub fn vector(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| self.unit()).collect()
    }
}

/// Upserts `n` random vectors `v0..v{n-1}` in batches, with metadata
/// `{"n": i, "category": "a"|"b"|"c", "even": bool}`. Returns the vectors.
pub async fn fill(
    svc: &VectorService,
    ctx: &AccessContext,
    coll: &str,
    field: &str,
    n: usize,
    dim: usize,
    seed: u64,
) -> Vec<Vec<f32>> {
    let mut rng = Rng::new(seed);
    let vectors: Vec<Vec<f32>> = (0..n).map(|_| rng.vector(dim)).collect();
    for (start, chunk) in vectors.chunks(1000).enumerate().map(|(i, c)| (i * 1000, c)) {
        let items = chunk
            .iter()
            .enumerate()
            .map(|(j, v)| {
                let i = start + j;
                let category = ["a", "b", "c"][i % 3];
                item(
                    &format!("v{i}"),
                    field,
                    v,
                    serde_json::json!({"n": i, "category": category, "even": i % 2 == 0}),
                )
            })
            .collect();
        svc.upsert(ctx, coll, items).await.unwrap();
    }
    vectors
}

pub async fn wait_ready(svc: &VectorService, ctx: &AccessContext, coll: &str) {
    assert!(
        svc.wait_ready(ctx, coll, Duration::from_secs(120)).await.unwrap(),
        "index did not become ready"
    );
}

pub fn ids(r: &SearchResponse) -> Vec<String> {
    r.hits.iter().map(|h| h.id.clone()).collect()
}
