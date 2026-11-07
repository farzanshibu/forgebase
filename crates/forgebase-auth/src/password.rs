//! Argon2id password hashing.
//!
//! Hashes are PHC strings (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`), so the
//! parameters travel with each hash: raising them later only affects new
//! hashes, and [`PasswordHasher::needs_rehash`] tells the login path to
//! upgrade old ones. [`PasswordHasher`] also implements the data core's
//! [`SecretHasher`], so `password` fields of every collection use it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordVerifier, Version};
use forgebase_data::SecretHasher;

use crate::AuthError;
use crate::crypto::fill_random;

/// Longest password accepted, in bytes (bounds hashing work per request).
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Config {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Iterations.
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u32,
}

impl Default for Argon2Config {
    /// OWASP 2024 baseline for Argon2id: 19 MiB, 2 iterations, 1 lane.
    fn default() -> Self {
        Self {
            memory_kib: 19 * 1024,
            iterations: 2,
            parallelism: 1,
        }
    }
}

impl Argon2Config {
    /// Cheap parameters for tests only (never in production).
    pub fn insecure_fast_for_tests() -> Self {
        Self {
            memory_kib: 64,
            iterations: 1,
            parallelism: 1,
        }
    }
}

/// Argon2id hasher and verifier.
#[derive(Debug, Clone)]
pub struct PasswordHasher {
    params: Params,
    /// A hash of a random password, verified when an account does not exist,
    /// so unknown and known accounts take the same time.
    dummy: Arc<str>,
    /// Hashes computed ahead of a data-core write (see [`PasswordHasher::prehash`]),
    /// keyed by a keyed digest of the password (the password itself is not kept).
    precomputed: Arc<Mutex<Precomputed>>,
    cache_key: Arc<[u8; 32]>,
}

type Precomputed = HashMap<[u8; 32], (String, Instant)>;

const PRECOMPUTED_TTL: Duration = Duration::from_secs(60);

impl PasswordHasher {
    /// Hasher with the given cost.
    pub fn new(cfg: Argon2Config) -> Result<Self, AuthError> {
        let params = Params::new(cfg.memory_kib, cfg.iterations, cfg.parallelism, None)
            .map_err(|e| AuthError::Internal(format!("invalid Argon2 parameters: {e}")))?;
        let mut key = [0u8; 32];
        fill_random(&mut key)?;
        let mut me = Self {
            params,
            dummy: Arc::from(""),
            precomputed: Arc::new(Mutex::new(HashMap::new())),
            cache_key: Arc::new(key),
        };
        let random = crate::crypto::random_token(24)?;
        me.dummy = Arc::from(me.hash(&random)?);
        Ok(me)
    }

    fn argon(&self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }

    /// Hashes a password (CPU-heavy: call from a blocking thread in async code).
    pub fn hash(&self, password: &str) -> Result<String, AuthError> {
        use argon2::PasswordHasher as _;
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(AuthError::invalid("password is too long"));
        }
        let mut salt = [0u8; 16];
        fill_random(&mut salt)?;
        let hash = self
            .argon()
            .hash_password_with_salt(password.as_bytes(), &salt)
            .map_err(|e| AuthError::Internal(format!("password hashing failed: {e}")))?;
        Ok(hash.to_string())
    }

    /// Checks a password against a stored hash. Unparseable hashes and
    /// non-Argon2id hashes never verify.
    pub fn verify(&self, password: &str, stored: &str) -> bool {
        if password.len() > MAX_PASSWORD_BYTES || !stored.starts_with("$argon2id$") {
            return false;
        }
        match PasswordHash::new(stored) {
            Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
            Err(_) => false,
        }
    }

    /// Spends the same time as a real verification, for unknown accounts.
    pub fn verify_dummy(&self, password: &str) {
        let _ = self.verify(password, &self.dummy.clone());
    }

    /// Whether `stored` was made with other parameters than the current ones.
    pub fn needs_rehash(&self, stored: &str) -> bool {
        match PasswordHash::new(stored) {
            Ok(parsed) => match Params::try_from(&parsed) {
                Ok(p) => {
                    p.m_cost() != self.params.m_cost()
                        || p.t_cost() != self.params.t_cost()
                        || p.p_cost() != self.params.p_cost()
                        || parsed.algorithm.as_str() != "argon2id"
                }
                Err(_) => true,
            },
            Err(_) => true,
        }
    }
}

impl PasswordHasher {
    fn cache_slot(&self, password: &str) -> [u8; 32] {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(&self.cache_key[..])
            .unwrap_or_else(|_| unreachable!("HMAC takes any key length"));
        mac.update(password.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Hashes `password` on a blocking thread and keeps the result for the
    /// next [`SecretHasher::hash`] call with the same password (within 60 s).
    ///
    /// The data core hashes password fields inside its write transaction,
    /// which holds the single writer; auth flows call this first so the
    /// expensive Argon2 work happens outside the transaction and off the
    /// async runtime.
    pub async fn prehash(&self, password: &str) -> Result<(), AuthError> {
        let me = self.clone();
        let pw = password.to_owned();
        let hash = tokio::task::spawn_blocking(move || me.hash(&pw))
            .await
            .map_err(|e| AuthError::Internal(format!("hashing task failed: {e}")))??;
        let slot = self.cache_slot(password);
        let mut map = self.precomputed.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        map.retain(|_, (_, at)| now.duration_since(*at) < PRECOMPUTED_TTL);
        map.insert(slot, (hash, now));
        Ok(())
    }

    /// Hashes on a blocking thread.
    pub async fn hash_async(&self, password: &str) -> Result<String, AuthError> {
        let me = self.clone();
        let pw = password.to_owned();
        tokio::task::spawn_blocking(move || me.hash(&pw))
            .await
            .map_err(|e| AuthError::Internal(format!("hashing task failed: {e}")))?
    }

    /// Verifies on a blocking thread. `stored = None` runs the dummy check.
    pub async fn verify_async(&self, password: &str, stored: Option<String>) -> bool {
        let me = self.clone();
        let pw = password.to_owned();
        tokio::task::spawn_blocking(move || match stored {
            Some(s) => me.verify(&pw, &s),
            None => {
                me.verify_dummy(&pw);
                false
            }
        })
        .await
        .unwrap_or(false)
    }
}

impl SecretHasher for PasswordHasher {
    fn hash(&self, secret: &str) -> Result<String, String> {
        let slot = self.cache_slot(secret);
        let cached = {
            let mut map = self.precomputed.lock().unwrap_or_else(|p| p.into_inner());
            map.remove(&slot)
        };
        match cached {
            Some((hash, at)) if at.elapsed() < PRECOMPUTED_TTL => Ok(hash),
            _ => PasswordHasher::hash(self, secret).map_err(|e| e.to_string()),
        }
    }
}

/// Checks password length rules before hashing. The minimum comes from the
/// collection's `password` field (default 8).
pub fn check_password_policy(password: &str, min_chars: usize) -> Result<(), AuthError> {
    let chars = password.chars().count();
    if chars < min_chars {
        return Err(AuthError::invalid(format!(
            "password must be at least {min_chars} characters"
        )));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(AuthError::invalid(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_verify_and_rehash() {
        let h = PasswordHasher::new(Argon2Config::insecure_fast_for_tests()).unwrap();
        let stored = h.hash("correct horse").unwrap();
        assert!(stored.starts_with("$argon2id$v=19$m=64,t=1,p=1$"), "{stored}");
        assert!(!stored.contains("correct horse"));
        assert!(h.verify("correct horse", &stored));
        assert!(!h.verify("wrong horse", &stored));
        assert!(!h.verify("correct horse", "plaintext"));
        assert!(!h.needs_rehash(&stored));
        let stronger = PasswordHasher::new(Argon2Config {
            memory_kib: 128,
            iterations: 1,
            parallelism: 1,
        })
        .unwrap();
        assert!(stronger.needs_rehash(&stored));
        assert!(stronger.verify("correct horse", &stored), "old params still verify");
        // Same password, different salt.
        assert_ne!(h.hash("correct horse").unwrap(), stored);
    }

    #[test]
    fn policy() {
        assert!(check_password_policy("short", 8).is_err());
        assert!(check_password_policy("long enough", 8).is_ok());
        assert!(check_password_policy(&"x".repeat(2000), 8).is_err());
    }
}
