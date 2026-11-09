//! The compatibility table: which commands the listener answers itself, which
//! it forwards to the engine, and which it refuses with a specific error.
//! `docs/adr/0013-resp.md` mirrors this file; the conformance tests drive
//! every entry over the wire.

/// Commands answered by the listener itself (connection state, server info).
pub const CONNECTION_COMMANDS: &[&str] = &[
    "AUTH",
    "HELLO",
    "PING",
    "ECHO",
    "SELECT",
    "QUIT",
    "RESET",
    "CLIENT",
    "COMMAND",
    "INFO",
    "TIME",
    "CONFIG",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "PSUBSCRIBE",
    "PUNSUBSCRIBE",
];

/// Data commands forwarded to the engine (`forgebase_kv::spec` knows each).
pub const DATA_COMMANDS: &[&str] = &[
    "DEL",
    "UNLINK",
    "EXISTS",
    "TYPE",
    "EXPIRE",
    "PEXPIRE",
    "EXPIREAT",
    "PEXPIREAT",
    "PERSIST",
    "TTL",
    "PTTL",
    "KEYS",
    "SCAN",
    "DBSIZE",
    "FLUSHDB",
    "GET",
    "SET",
    "SETNX",
    "SETEX",
    "PSETEX",
    "GETDEL",
    "APPEND",
    "STRLEN",
    "MGET",
    "MSET",
    "INCR",
    "DECR",
    "INCRBY",
    "DECRBY",
    "INCRBYFLOAT",
    "HSET",
    "HSETNX",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HDEL",
    "HINCRBY",
    "LPUSH",
    "RPUSH",
    "LPOP",
    "RPOP",
    "LLEN",
    "LRANGE",
    "LINDEX",
    "LSET",
    "LREM",
    "LTRIM",
    "SADD",
    "SREM",
    "SISMEMBER",
    "SMEMBERS",
    "SCARD",
    "SPOP",
    "SUNION",
    "SINTER",
    "SDIFF",
    "ZADD",
    "ZREM",
    "ZSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZRANGE",
    "ZREVRANGE",
    "ZRANGEBYSCORE",
    "ZINCRBY",
    "ZCARD",
    "ZCOUNT",
    "ZPOPMIN",
    "ZPOPMAX",
    "XADD",
    "XLEN",
    "XRANGE",
    "XREVRANGE",
    "XREAD",
    "XDEL",
    "XTRIM",
    "PUBLISH",
];

const TXN: &str = "ERR transactions (MULTI/EXEC/WATCH) are not supported";
const SCRIPT: &str = "ERR scripting (EVAL/FUNCTION) is not supported";
const BLOCKING: &str = "ERR blocking commands are not supported";
const GROUPS: &str = "ERR stream consumer groups are not supported";
const CLUSTER: &str = "ERR replication and cluster commands are not supported";
const ADMIN: &str = "ERR server administration commands are not supported";
const PUBSUB: &str = "ERR this pub/sub command is not supported";
const FLUSHALL: &str = "ERR FLUSHALL is not supported; use FLUSHDB to clear the selected namespace";

/// Known Redis commands refused with a reason. Not an exhaustive Redis
/// command list: anything not listed here and not supported answers
/// `ERR unknown command`.
pub const UNSUPPORTED: &[(&str, &[&str])] = &[
    (TXN, &["MULTI", "EXEC", "DISCARD", "WATCH", "UNWATCH"]),
    (
        SCRIPT,
        &[
            "EVAL",
            "EVALSHA",
            "EVAL_RO",
            "EVALSHA_RO",
            "SCRIPT",
            "FUNCTION",
            "FCALL",
            "FCALL_RO",
        ],
    ),
    (
        BLOCKING,
        &[
            "BLPOP",
            "BRPOP",
            "BLMOVE",
            "BRPOPLPUSH",
            "BLMPOP",
            "BZPOPMIN",
            "BZPOPMAX",
            "BZMPOP",
        ],
    ),
    (
        GROUPS,
        &[
            "XGROUP",
            "XREADGROUP",
            "XACK",
            "XCLAIM",
            "XAUTOCLAIM",
            "XPENDING",
            "XINFO",
        ],
    ),
    (
        CLUSTER,
        &[
            "REPLICAOF",
            "SLAVEOF",
            "SYNC",
            "PSYNC",
            "WAIT",
            "WAITAOF",
            "CLUSTER",
            "READONLY",
            "READWRITE",
            "ASKING",
            "FAILOVER",
        ],
    ),
    (
        ADMIN,
        &[
            "SAVE",
            "BGSAVE",
            "BGREWRITEAOF",
            "LASTSAVE",
            "SHUTDOWN",
            "DEBUG",
            "MONITOR",
            "SLOWLOG",
            "MEMORY",
            "ACL",
            "MODULE",
            "LATENCY",
            "SWAPDB",
            "MIGRATE",
            "MOVE",
        ],
    ),
    (PUBSUB, &["PUBSUB", "SSUBSCRIBE", "SUNSUBSCRIBE", "SPUBLISH"]),
    (FLUSHALL, &["FLUSHALL"]),
];

/// Known Redis commands the engine does not implement: refused with
/// `ERR command '<name>' is not supported by ForgeBase KV`.
pub const NOT_IMPLEMENTED: &[&str] = &[
    // keys
    "RENAME",
    "RENAMENX",
    "COPY",
    "RANDOMKEY",
    "SORT",
    "SORT_RO",
    "OBJECT",
    "DUMP",
    "RESTORE",
    "TOUCH",
    "EXPIRETIME",
    "PEXPIRETIME",
    // strings
    "GETEX",
    "GETSET",
    "GETRANGE",
    "SUBSTR",
    "SETRANGE",
    "MSETNX",
    "LCS",
    // bitmaps, hyperloglog, geo
    "SETBIT",
    "GETBIT",
    "BITCOUNT",
    "BITOP",
    "BITPOS",
    "BITFIELD",
    "PFADD",
    "PFCOUNT",
    "PFMERGE",
    "GEOADD",
    "GEODIST",
    "GEOHASH",
    "GEOPOS",
    "GEORADIUS",
    "GEOSEARCH",
    // hashes
    "HINCRBYFLOAT",
    "HRANDFIELD",
    "HSCAN",
    "HSTRLEN",
    // lists
    "LPUSHX",
    "RPUSHX",
    "LINSERT",
    "LPOS",
    "LMOVE",
    "RPOPLPUSH",
    "LMPOP",
    // sets
    "SMOVE",
    "SINTERSTORE",
    "SUNIONSTORE",
    "SDIFFSTORE",
    "SINTERCARD",
    "SRANDMEMBER",
    "SMISMEMBER",
    "SSCAN",
    // sorted sets
    "ZMSCORE",
    "ZRANDMEMBER",
    "ZSCAN",
    "ZUNION",
    "ZUNIONSTORE",
    "ZINTER",
    "ZINTERSTORE",
    "ZDIFF",
    "ZDIFFSTORE",
    "ZRANGESTORE",
    "ZRANGEBYLEX",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZLEXCOUNT",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREMRANGEBYLEX",
    "ZMPOP",
    // streams
    "XSETID",
];

/// How the listener treats a command name (upper case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Answered by the listener or forwarded to the engine.
    Supported,
    /// Refused with this error line.
    Refused(&'static str),
    /// Not a command the listener knows.
    Unknown,
}

/// Classifies `name`.
pub fn classify(name: &str) -> Support {
    if CONNECTION_COMMANDS.contains(&name) || DATA_COMMANDS.contains(&name) {
        return Support::Supported;
    }
    for (msg, names) in UNSUPPORTED {
        if names.contains(&name) {
            return Support::Refused(msg);
        }
    }
    if NOT_IMPLEMENTED.contains(&name) {
        return Support::Refused("");
    }
    Support::Unknown
}

/// The error line for a refused command.
pub fn refusal(name: &str, msg: &str) -> String {
    if msg.is_empty() {
        format!("ERR command '{name}' is not supported by ForgeBase KV")
    } else {
        msg.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lists_do_not_overlap_and_data_commands_exist_in_the_engine() {
        let mut seen = std::collections::HashSet::new();
        for n in CONNECTION_COMMANDS
            .iter()
            .chain(DATA_COMMANDS)
            .chain(NOT_IMPLEMENTED)
            .chain(UNSUPPORTED.iter().flat_map(|(_, n)| n.iter()))
        {
            assert!(seen.insert(*n), "{n} listed twice");
        }
        for n in DATA_COMMANDS {
            assert!(forgebase_kv::spec(n).is_some(), "{n} has no engine spec");
        }
        // PING is answered here (it behaves differently while subscribed) but
        // the engine knows it too.
        assert!(forgebase_kv::spec("PING").is_some());
    }

    #[test]
    fn classify_covers_the_four_outcomes() {
        assert_eq!(classify("GET"), Support::Supported);
        assert_eq!(classify("AUTH"), Support::Supported);
        assert_eq!(classify("MULTI"), Support::Refused(TXN));
        assert_eq!(classify("GETRANGE"), Support::Refused(""));
        assert_eq!(classify("NOPE"), Support::Unknown);
        assert_eq!(
            refusal("GETRANGE", ""),
            "ERR command 'GETRANGE' is not supported by ForgeBase KV"
        );
    }
}
