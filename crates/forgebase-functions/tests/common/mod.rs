//! Test harness: a real SQLite database migrated to 0016, the real policy
//! engine as the authorizer, the KV engine, realtime (for the outbox that
//! feeds after-hooks), and the functions engine; plus a tiny WAT builder for
//! guest modules.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use base64::Engine as _;
use forgebase_auth::crypto::Keyring;
use forgebase_data::db::Database;
use forgebase_data::db::testing::{TestDb, test_backend};
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Authorizer, DataService};
use forgebase_functions::runtime::HOST_FUNCTIONS;
use forgebase_functions::{DeployRequest, FunctionService, FunctionsConfig, LimitsInput};
use forgebase_kv::{KvConfig, KvService};
use forgebase_policy::PolicyService;
use forgebase_policy::snapshot::Effect;
use forgebase_realtime::{RealtimeConfig, RealtimeService};
use serde_json::{Value as Json, json};

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub struct H {
    pub svc: FunctionService,
    pub data: DataService,
    pub policy: PolicyService,
    pub kv: KvService,
    pub rt: RealtimeService,
    pub db: Database,
    pub dir: tempfile::TempDir,
    pub test: TestDb,
}

impl Drop for H {
    fn drop(&mut self) {
        self.svc.shutdown();
        self.rt.shutdown();
    }
}

pub async fn harness() -> H {
    harness_with(FunctionsConfig::default()).await
}

pub async fn harness_with(cfg: FunctionsConfig) -> H {
    let dir = tempfile::tempdir().unwrap();
    let test = test_backend().await;
    let db = test.database().clone();
    let backend: Arc<dyn forgebase_data::db::Backend> = Arc::new(db.clone());
    let policy = PolicyService::open(backend.clone()).await.unwrap();
    let authorizer: Arc<dyn Authorizer> = policy.authorizer();
    let data = DataService::new(backend.clone(), authorizer.clone()).await.unwrap();
    policy.attach_data(data.clone());
    let kv = KvService::open(KvConfig::default(), authorizer, None).await.unwrap();
    let rt = RealtimeService::start(data.clone(), None, RealtimeConfig::default(), None)
        .await
        .unwrap();
    let keyring = Keyring::load_or_create(&dir.path().join("master.key")).unwrap();
    let svc = FunctionService::open(data.clone(), Some(kv.clone()), keyring, cfg)
        .await
        .unwrap();
    H {
        svc,
        data,
        policy,
        kv,
        rt,
        db,
        dir,
        test,
    }
}

impl H {
    /// Creates a collection as administrator.
    pub async fn collection(&self, def: Json) -> Collection {
        let c: Collection = serde_json::from_value(def).unwrap();
        self.data.create_collection(&admin(), c).await.unwrap()
    }

    /// Grants `permission` to function `name`'s identity (realm-wide override).
    pub async fn grant(&self, function: &str, permission: &str) {
        let principal = forgebase_functions::identity(function).principal_key();
        self.policy
            .add_override(&admin(), None, &principal, permission, Effect::Allow)
            .await
            .unwrap();
    }

    /// Deploys as administrator.
    pub async fn deploy(&self, req: DeployRequest) -> forgebase_functions::Deployed {
        self.svc.deploy(&admin(), req).await.unwrap()
    }
}

/// A deploy request for `wasm` (base64) with capabilities.
pub fn req(name: &str, wasm: &str, caps: &[&str]) -> DeployRequest {
    DeployRequest {
        name: name.into(),
        wasm: Some(wasm.into()),
        source: None,
        language: None,
        description: None,
        access: None,
        capabilities: caps.iter().map(|c| (*c).to_owned()).collect(),
        fetch_hosts: Vec::new(),
        limits: LimitsInput::default(),
        env: Default::default(),
        hooks: Vec::new(),
        activate: true,
    }
}

/// Buffers in guest memory (each 64 KiB).
pub const IN: u32 = 0x10000;
pub const RES: u32 = 0x20000;
pub const ARGS: u32 = 0x30000;
pub const OUT: u32 = 0x40000;
pub const AUX: u32 = 0x50000;

/// Builds a guest module: imports, string constants in a data segment, the
/// helpers `$copy(dst, src, len) -> end`, `$contains(h, hl, n, nl) -> i32`,
/// `$emit(ptr, len)`, and `handle` with locals `$n $r $p $q $k $i`.
pub struct Wat {
    imports: Vec<&'static str>,
    data: Vec<(u32, Vec<u8>)>,
    next: u32,
    pub pages: u32,
    pub extra: String,
}

impl Wat {
    pub fn new(imports: &[&'static str]) -> Self {
        let mut all = vec!["input_len", "input_read", "output", "result_read"];
        for i in imports {
            if !all.contains(i) {
                all.push(i);
            }
        }
        Self {
            imports: all,
            data: Vec::new(),
            next: 16,
            pages: 6,
            extra: String::new(),
        }
    }

    /// A string constant: `(i32.const ptr) (i32.const len)`.
    pub fn s(&mut self, text: &str) -> String {
        let ptr = self.next;
        let bytes = text.as_bytes().to_vec();
        let len = bytes.len() as u32;
        self.next += len + 8;
        assert!(self.next < IN, "constants overflow");
        self.data.push((ptr, bytes));
        format!("(i32.const {ptr}) (i32.const {len})")
    }

    pub fn module(&self, body: &str) -> String {
        let mut out = String::from("(module\n");
        for name in &self.imports {
            let spec = HOST_FUNCTIONS.iter().find(|h| h.name == *name).expect("host function");
            let params = vec!["i32"; spec.params].join(" ");
            out.push_str(&format!(
                "  (import \"forgebase\" \"{name}\" (func ${name} (param {params}) (result i32)))\n"
            ));
        }
        out.push_str(&format!("  (memory (export \"memory\") {})\n", self.pages));
        for (ptr, bytes) in &self.data {
            let esc: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
            out.push_str(&format!("  (data (i32.const {ptr}) \"{esc}\")\n"));
        }
        out.push_str(
            r#"
  (func $copy (param $dst i32) (param $src i32) (param $len i32) (result i32)
    (memory.copy (local.get $dst) (local.get $src) (local.get $len))
    (i32.add (local.get $dst) (local.get $len)))
  (func $emit (param $p i32) (param $l i32)
    (drop (call $output (local.get $p) (local.get $l))))
  (func $contains (param $h i32) (param $hl i32) (param $nd i32) (param $nl i32) (result i32)
    (local $i i32) (local $j i32)
    (block $done
      (loop $outer
        (br_if $done (i32.gt_s (local.get $nl) (i32.sub (local.get $hl) (local.get $i))))
        (local.set $j (i32.const 0))
        (block $mismatch
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $nl)) (then (return (i32.const 1))))
            (br_if $mismatch
              (i32.ne
                (i32.load8_u (i32.add (local.get $h) (i32.add (local.get $i) (local.get $j))))
                (i32.load8_u (i32.add (local.get $nd) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
"#,
        );
        out.push_str(&self.extra);
        out.push_str(&format!(
            "  (func (export \"handle\") (result i32)\n    (local $n i32) (local $r i32) (local $p i32) (local $q i32) (local $k i32) (local $i i32)\n{body}\n  )\n)\n"
        ));
        out
    }

    /// The module as base64 binary.
    pub fn wasm(&self, body: &str) -> String {
        let text = self.module(body);
        let bin = wat::parse_str(&text).unwrap_or_else(|e| panic!("bad WAT: {e}\n{text}"));
        base64::engine::general_purpose::STANDARD.encode(bin)
    }
}

/// Raw WAT text as base64 binary.
pub fn wat_b64(text: &str) -> String {
    let bin = wat::parse_str(text).unwrap_or_else(|e| panic!("bad WAT: {e}\n{text}"));
    base64::engine::general_purpose::STANDARD.encode(bin)
}

/// A module whose output is the constant `text`.
pub fn constant(text: &str) -> String {
    let mut w = Wat::new(&[]);
    let s = w.s(text);
    w.wasm(&format!("(call $emit {s}) (i32.const 0)"))
}

/// Reads the input into IN, sets `$n`.
pub const READ_INPUT: &str = "(local.set $n (call $input_read (i32.const 65536) (call $input_len)))";

/// JSON of an execution's output.
pub fn out_json(o: &forgebase_functions::Outcome) -> Json {
    o.json().unwrap().unwrap_or(json!(null))
}
