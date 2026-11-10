//! Engine errors as RESP error lines, with the error codes Redis clients
//! branch on (`WRONGTYPE`, `NOAUTH`, `NOPERM`, `OOM`, ...).

use forgebase_kv::KvError;

fn quoted(args: &[Vec<u8>]) -> String {
    args.iter()
        .take(3)
        .map(|a| {
            let s: String = String::from_utf8_lossy(a).chars().take(64).collect();
            format!("'{s}'")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ERR unknown command 'x', with args beginning with: 'a' 'b'`.
pub(crate) fn unknown_command(cmd: &[Vec<u8>]) -> String {
    let name: String = String::from_utf8_lossy(&cmd[0]).chars().take(64).collect();
    format!(
        "ERR unknown command '{name}', with args beginning with: {}",
        quoted(&cmd[1..])
    )
}

/// `ERR wrong number of arguments for 'x' command`.
pub(crate) fn wrong_arity(name: &str) -> String {
    format!(
        "ERR wrong number of arguments for '{}' command",
        name.to_ascii_lowercase()
    )
}

/// The error line for an engine error on command `cmd`.
pub(crate) fn kv_error(e: &KvError, cmd: &[Vec<u8>]) -> String {
    let name = String::from_utf8_lossy(&cmd[0]).to_ascii_lowercase();
    match e {
        KvError::WrongType => "WRONGTYPE Operation against a key holding the wrong kind of value".into(),
        KvError::NotFloat => "ERR value is not a valid float".into(),
        KvError::UnknownCommand(_) => unknown_command(cmd),
        KvError::WrongArity(n) => wrong_arity(n),
        KvError::OutOfMemory => "OOM command not allowed when used memory > 'maxmemory'.".into(),
        KvError::Unauthenticated => "NOAUTH Authentication required.".into(),
        KvError::Forbidden => format!("NOPERM this user has no permissions to run the '{name}' command"),
        KvError::Storage(m) => {
            tracing::warn!(error = %m, command = %name, "kv storage error over RESP");
            "ERR internal storage error".into()
        }
        KvError::TooManyChannels => "ERR too many pub/sub channels".into(),
        KvError::TenantRequired(_) => {
            "NOPERM tenant_required: this key has no tenant; bind it to one, or SELECT a shared.<name> namespace".into()
        }
        other => format!("ERR {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(a: &[&str]) -> Vec<Vec<u8>> {
        a.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn codes_match_what_redis_clients_expect() {
        let cmd = c(&["LPUSH", "k", "v"]);
        assert!(kv_error(&KvError::WrongType, &cmd).starts_with("WRONGTYPE "));
        assert!(kv_error(&KvError::Forbidden, &cmd).starts_with("NOPERM "));
        assert!(kv_error(&KvError::Unauthenticated, &cmd).starts_with("NOAUTH "));
        assert!(kv_error(&KvError::OutOfMemory, &cmd).starts_with("OOM "));
        assert_eq!(
            kv_error(&KvError::NotInteger, &cmd),
            "ERR value is not an integer or out of range"
        );
        assert_eq!(
            kv_error(&KvError::WrongArity("get".into()), &cmd),
            "ERR wrong number of arguments for 'get' command"
        );
        assert_eq!(
            unknown_command(&c(&["FOO", "a", "b"])),
            "ERR unknown command 'FOO', with args beginning with: 'a' 'b'"
        );
    }

    #[test]
    fn storage_errors_do_not_leak_details() {
        let msg = kv_error(
            &KvError::Storage("sqlite: disk I/O error at /secret/path".into()),
            &c(&["GET", "k"]),
        );
        assert_eq!(msg, "ERR internal storage error");
    }
}
