// ForgeBase function prelude: Web-standard globals and the `forgebase`
// module, evaluated in every execution before the function's code
// (ADR-0027). The value of the last expression is the engine's internal API
// ({run, tick}); everything else is installed on globalThis.
(() => {
  "use strict";
  const N = globalThis.__forgebase_native;
  delete globalThis.__forgebase_native;

  const define = (obj, name, value) =>
    Object.defineProperty(obj, name, { value, writable: true, configurable: true, enumerable: false });
  const isBytes = (v) => v instanceof ArrayBuffer || ArrayBuffer.isView(v);
  const toU8 = (v) =>
    v instanceof Uint8Array ? v : v instanceof ArrayBuffer ? new Uint8Array(v) : new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
  const copyBuffer = (u8) => u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);

  // ------------------------------------------------------------- host calls

  class ForgeBaseError extends Error {
    constructor(code, message) {
      super(message);
      this.name = "ForgeBaseError";
      this.code = code;
    }
  }

  function host(op, arg) {
    try {
      return N.call(op, arg === undefined ? "" : JSON.stringify(arg));
    } catch (e) {
      if (e && typeof e.hostError === "string") {
        let j;
        try {
          j = JSON.parse(e.hostError);
        } catch {
          j = { code: "host_error", message: e.hostError };
        }
        throw new ForgeBaseError(String(j.code || "host_error"), String(j.message || "host call failed"));
      }
      throw e;
    }
  }
  const hostJson = (op, arg) => {
    const s = host(op, arg);
    return s === "" ? null : JSON.parse(s);
  };
  // Asynchronous facade: host calls complete synchronously inside the
  // sandbox, but the API is promise-based like other runtimes.
  const call = (op, arg) => new Promise((resolve) => resolve(hostJson(op, arg)));

  // ---------------------------------------------------------------- console

  function inspect(v, seen = new Set(), depth = 0) {
    if (typeof v === "string") return depth ? JSON.stringify(v) : v;
    if (v === null || typeof v !== "object") {
      if (typeof v === "function") return `[Function ${v.name || "(anonymous)"}]`;
      if (typeof v === "bigint") return `${v}n`;
      if (typeof v === "symbol") return v.toString();
      return String(v);
    }
    if (v instanceof Error) return v.stack ? `${v.name}: ${v.message}\n${v.stack}`.trimEnd() : `${v.name}: ${v.message}`;
    if (seen.has(v)) return "[Circular]";
    if (depth > 4) return Array.isArray(v) ? "[Array]" : "[Object]";
    seen.add(v);
    try {
      if (Array.isArray(v)) return `[${v.map((x) => inspect(x, seen, depth + 1)).join(", ")}]`;
      if (v instanceof Map) return `Map(${v.size}) {${[...v].map(([k, x]) => `${inspect(k, seen, depth + 1)} => ${inspect(x, seen, depth + 1)}`).join(", ")}}`;
      if (v instanceof Set) return `Set(${v.size}) {${[...v].map((x) => inspect(x, seen, depth + 1)).join(", ")}}`;
      if (v instanceof Date) return v.toISOString();
      if (ArrayBuffer.isView(v)) return `${v.constructor.name}(${v.length ?? v.byteLength}) [${Array.from(v.length !== undefined ? v : new Uint8Array(v.buffer, v.byteOffset, v.byteLength)).slice(0, 50).join(", ")}${(v.length ?? v.byteLength) > 50 ? ", ..." : ""}]`;
      if (typeof v.toJSON === "function") return inspect(v.toJSON(), seen, depth);
      const name = v.constructor && v.constructor !== Object ? `${v.constructor.name} ` : "";
      const body = Object.keys(v).map((k) => `${/^[A-Za-z_$][\w$]*$/.test(k) ? k : JSON.stringify(k)}: ${inspect(v[k], seen, depth + 1)}`);
      return `${name}{${body.length ? ` ${body.join(", ")} ` : ""}}`;
    } finally {
      seen.delete(v);
    }
  }
  function format(args) {
    if (typeof args[0] === "string" && args.length > 1 && /%[sdifoOj%]/.test(args[0])) {
      let i = 1;
      const head = args[0].replace(/%([sdifoOj%])/g, (m, f) => {
        if (f === "%") return "%";
        if (i >= args.length) return m;
        const a = args[i++];
        if (f === "s") return typeof a === "string" ? a : inspect(a, new Set(), 1);
        if (f === "d" || f === "i") return String(f === "i" ? Math.trunc(Number(a)) : Number(a));
        if (f === "f") return String(Number(a));
        if (f === "j") return JSON.stringify(a);
        return inspect(a, new Set(), 1);
      });
      return [head, ...args.slice(i).map((a) => inspect(a))].join(" ");
    }
    return args.map((a) => inspect(a)).join(" ");
  }
  const counts = new Map();
  const timersByLabel = new Map();
  const log = (level) => (...args) => N.log(level, format(args));
  const console = {
    debug: log(0),
    log: log(1),
    info: log(1),
    warn: log(2),
    error: log(3),
    trace: (...args) => N.log(0, `${format(args)}\n${new Error().stack || ""}`.trimEnd()),
    assert: (cond, ...args) => {
      if (!cond) N.log(3, `Assertion failed${args.length ? `: ${format(args)}` : ""}`);
    },
    dir: (v) => N.log(1, inspect(v, new Set(), 1)),
    table: (v) => N.log(1, inspect(v, new Set(), 1)),
    count: (label = "default") => {
      const n = (counts.get(label) || 0) + 1;
      counts.set(label, n);
      N.log(1, `${label}: ${n}`);
    },
    countReset: (label = "default") => counts.delete(label),
    time: (label = "default") => timersByLabel.set(label, Date.now()),
    timeEnd: (label = "default") => {
      const t = timersByLabel.get(label);
      timersByLabel.delete(label);
      if (t !== undefined) N.log(1, `${label}: ${Date.now() - t}ms`);
    },
    timeLog: (label = "default", ...args) => {
      const t = timersByLabel.get(label);
      if (t !== undefined) N.log(1, `${label}: ${Date.now() - t}ms${args.length ? ` ${format(args)}` : ""}`);
    },
    group: () => {},
    groupEnd: () => {},
  };
  define(globalThis, "console", console);

  // ------------------------------------------- per-execution random and time

  // The engine's state is snapshotted once and copied into every execution,
  // so the per-execution parts are set up in begin(): a fresh Math.random
  // seed (xoshiro128**, seeded from the host CSPRNG) and the origin of
  // performance.now().
  let rs = new Uint32Array(4);
  const rotl = (x, k) => (x << k) | (x >>> (32 - k));
  const nextU32 = () => {
    const r = Math.imul(rotl(Math.imul(rs[1], 5), 7), 9) >>> 0;
    const t = rs[1] << 9;
    rs[2] ^= rs[0];
    rs[3] ^= rs[1];
    rs[1] ^= rs[2];
    rs[0] ^= rs[3];
    rs[2] ^= t;
    rs[3] = rotl(rs[3], 11);
    return r;
  };
  define(Math, "random", function random() {
    return (nextU32() * 2 ** 21 + (nextU32() >>> 11)) / 2 ** 53;
  });
  let perfOrigin = 0;
  let timeOrigin = 0;
  const performance = {
    now: () => N.now() - perfOrigin,
    get timeOrigin() {
      return timeOrigin;
    },
    toJSON: () => ({ timeOrigin }),
  };
  define(globalThis, "performance", performance);
  function begin() {
    const seed = N.random(16);
    rs = new Uint32Array(seed.buffer.slice(0));
    if ((rs[0] | rs[1] | rs[2] | rs[3]) === 0) rs[0] = 1;
    perfOrigin = N.now();
    timeOrigin = Date.now();
  }

  // ----------------------------------------------------------------- timers

  let timerSeq = 0;
  const timers = new Map();
  function addTimer(fn, ms, args, repeat) {
    if (typeof fn !== "function") throw new TypeError("the callback must be a function");
    const delay = Math.max(0, Number(ms) || 0);
    const id = ++timerSeq;
    timers.set(id, { at: Date.now() + delay, fn, args, repeat: repeat ? Math.max(1, delay) : 0, seq: id });
    return id;
  }
  define(globalThis, "setTimeout", (fn, ms, ...args) => addTimer(fn, ms, args, false));
  define(globalThis, "setInterval", (fn, ms, ...args) => addTimer(fn, ms, args, true));
  define(globalThis, "clearTimeout", (id) => void timers.delete(id));
  define(globalThis, "clearInterval", (id) => void timers.delete(id));

  // Sleeps until the earliest timer is due and runs it; false when no timer is left.
  function tick() {
    if (timers.size === 0) return false;
    let next = null;
    for (const [id, t] of timers) if (!next || t.at < next[1].at || (t.at === next[1].at && t.seq < next[1].seq)) next = [id, t];
    const wait = next[1].at - Date.now();
    if (wait > 0) host("sleep", { ms: Math.ceil(wait) });
    const [id, t] = next;
    if (t.repeat) {
      t.at = Date.now() + t.repeat;
      t.seq = ++timerSeq;
    } else {
      timers.delete(id);
    }
    t.fn(...t.args);
    return true;
  }

  // -------------------------------------------------------- text and bytes

  class TextEncoder {
    get encoding() {
      return "utf-8";
    }
    encode(input = "") {
      return N.utf8encode(String(input));
    }
    encodeInto(input, dest) {
      const bytes = N.utf8encode(String(input));
      // Write whole characters only.
      let n = Math.min(bytes.length, dest.length);
      while (n > 0 && n < bytes.length && (bytes[n] & 0xc0) === 0x80) n--;
      dest.set(bytes.subarray(0, n));
      const read = N.utf8decode(bytes.subarray(0, n), false, true).length;
      return { read, written: n };
    }
  }
  class TextDecoder {
    #fatal;
    #ignoreBOM;
    constructor(label = "utf-8", options = {}) {
      const l = String(label).trim().toLowerCase();
      if (l !== "utf-8" && l !== "utf8" && l !== "unicode-1-1-utf-8") throw new RangeError(`the encoding ${label} is not supported (only utf-8)`);
      this.#fatal = !!options.fatal;
      this.#ignoreBOM = !!options.ignoreBOM;
    }
    get encoding() {
      return "utf-8";
    }
    get fatal() {
      return this.#fatal;
    }
    get ignoreBOM() {
      return this.#ignoreBOM;
    }
    decode(input) {
      if (input === undefined) return "";
      if (!isBytes(input)) throw new TypeError("decode() takes an ArrayBuffer or a typed array");
      return N.utf8decode(toU8(input), this.#fatal, this.#ignoreBOM);
    }
  }
  define(globalThis, "TextEncoder", TextEncoder);
  define(globalThis, "TextDecoder", TextDecoder);
  const utf8 = (s) => N.utf8encode(String(s));
  const fromUtf8 = (b) => N.utf8decode(b, false, false);

  // ---------------------------------------------------------- structuredClone

  function structuredClone(value) {
    const seen = new Map();
    const fail = (what) => {
      throw new DOMException(`${what} could not be cloned`, "DataCloneError");
    };
    const clone = (v) => {
      if (v === null || (typeof v !== "object" && typeof v !== "function" && typeof v !== "symbol")) return v;
      if (typeof v === "function") fail("a function");
      if (typeof v === "symbol") fail("a symbol");
      if (seen.has(v)) return seen.get(v);
      let out;
      if (v instanceof Date) out = new Date(v.getTime());
      else if (v instanceof RegExp) out = new RegExp(v.source, v.flags);
      else if (v instanceof ArrayBuffer) out = v.slice(0);
      else if (v instanceof DataView) out = new DataView(clone(v.buffer), v.byteOffset, v.byteLength);
      else if (ArrayBuffer.isView(v)) out = new v.constructor(clone(v.buffer), v.byteOffset, v.length);
      else if (v instanceof Map) {
        out = new Map();
        seen.set(v, out);
        for (const [k, x] of v) out.set(clone(k), clone(x));
        return out;
      } else if (v instanceof Set) {
        out = new Set();
        seen.set(v, out);
        for (const x of v) out.add(clone(x));
        return out;
      } else if (v instanceof Error) {
        out = new (globalThis[v.name] && globalThis[v.name].prototype instanceof Error ? globalThis[v.name] : Error)(v.message);
        if (v.stack) out.stack = v.stack;
      } else if (Array.isArray(v)) {
        out = new Array(v.length);
        seen.set(v, out);
        for (let i = 0; i < v.length; i++) if (i in v) out[i] = clone(v[i]);
        return out;
      } else if (v instanceof Promise || v instanceof WeakMap || v instanceof WeakSet) fail(v.constructor.name);
      else {
        out = {};
        seen.set(v, out);
        for (const k of Object.keys(v)) out[k] = clone(v[k]);
        return out;
      }
      seen.set(v, out);
      return out;
    };
    return clone(value);
  }
  define(globalThis, "structuredClone", structuredClone);

  // -------------------------------------------------------------------- URL

  const FORM_SAFE = /[A-Za-z0-9*\-._]/;
  function formEncode(s) {
    let out = "";
    for (const b of utf8(s)) {
      const c = String.fromCharCode(b);
      if (b === 0x20) out += "+";
      else if (b < 0x80 && FORM_SAFE.test(c)) out += c;
      else out += `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
    }
    return out;
  }
  function percentDecode(s) {
    const bytes = [];
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 0x25 && i + 2 < s.length && /^[0-9A-Fa-f]{2}$/.test(s.slice(i + 1, i + 3))) {
        bytes.push(parseInt(s.slice(i + 1, i + 3), 16));
        i += 2;
      } else if (c < 0x80) bytes.push(c);
      else for (const b of utf8(s[i] + (c >= 0xd800 && c < 0xdc00 && i + 1 < s.length ? s[++i] : ""))) bytes.push(b);
    }
    return fromUtf8(new Uint8Array(bytes));
  }
  const formDecode = (s) => percentDecode(s.replace(/\+/g, " "));

  class URLSearchParams {
    #list = [];
    #url = null;
    constructor(init = "") {
      if (init instanceof URLSearchParams) this.#list = init.#list.map(([k, v]) => [k, v]);
      else if (typeof init === "object" && init !== null) {
        if (typeof init[Symbol.iterator] === "function") {
          for (const pair of init) {
            const p = [...pair];
            if (p.length !== 2) throw new TypeError("each pair must have exactly two items");
            this.#list.push([String(p[0]), String(p[1])]);
          }
        } else for (const k of Object.keys(init)) this.#list.push([k, String(init[k])]);
      } else this.#parse(String(init));
    }
    #parse(s) {
      this.#list = [];
      if (s.startsWith("?")) s = s.slice(1);
      for (const part of s.split("&")) {
        if (!part) continue;
        const i = part.indexOf("=");
        this.#list.push(i < 0 ? [formDecode(part), ""] : [formDecode(part.slice(0, i)), formDecode(part.slice(i + 1))]);
      }
    }
    static _link(p, url, query) {
      p.#url = url;
      p.#parse(query);
    }
    static _reparse(p, query) {
      p.#parse(query);
    }
    #update() {
      if (this.#url) this.#url._setSearchFromParams(this.toString());
    }
    get size() {
      return this.#list.length;
    }
    append(k, v) {
      this.#list.push([String(k), String(v)]);
      this.#update();
    }
    delete(k, v) {
      k = String(k);
      this.#list = this.#list.filter(([a, b]) => a !== k || (v !== undefined && b !== String(v)));
      this.#update();
    }
    get(k) {
      k = String(k);
      const e = this.#list.find(([a]) => a === k);
      return e ? e[1] : null;
    }
    getAll(k) {
      k = String(k);
      return this.#list.filter(([a]) => a === k).map(([, b]) => b);
    }
    has(k, v) {
      k = String(k);
      return this.#list.some(([a, b]) => a === k && (v === undefined || b === String(v)));
    }
    set(k, v) {
      k = String(k);
      v = String(v);
      const i = this.#list.findIndex(([a]) => a === k);
      if (i < 0) this.#list.push([k, v]);
      else {
        this.#list[i][1] = v;
        this.#list = this.#list.filter(([a], j) => a !== k || j === i);
      }
      this.#update();
    }
    sort() {
      this.#list = this.#list
        .map((e, i) => [e, i])
        .sort((x, y) => (x[0][0] < y[0][0] ? -1 : x[0][0] > y[0][0] ? 1 : x[1] - y[1]))
        .map(([e]) => e);
      this.#update();
    }
    forEach(fn, thisArg) {
      for (const [k, v] of this.#list) fn.call(thisArg, v, k, this);
    }
    *entries() {
      for (const [k, v] of this.#list) yield [k, v];
    }
    *keys() {
      for (const [k] of this.#list) yield k;
    }
    *values() {
      for (const [, v] of this.#list) yield v;
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    toString() {
      return this.#list.map(([k, v]) => `${formEncode(k)}=${formEncode(v)}`).join("&");
    }
    get [Symbol.toStringTag]() {
      return "URLSearchParams";
    }
  }

  const SPECIAL = { "http:": "80", "https:": "443", "ws:": "80", "wss:": "443", "ftp:": "21", "file:": "" };
  const encodeSet = (s, keep) =>
    s.replace(/[\s\S]/gu, (c) => {
      if (keep.test(c)) return c;
      return Array.from(utf8(c), (b) => `%${b.toString(16).toUpperCase().padStart(2, "0")}`).join("");
    });
  const PATH_KEEP = /[A-Za-z0-9\-._~!$&'()*+,;=:@%\/]/;
  const QUERY_KEEP = /[A-Za-z0-9\-._~!$&'()*+,;=:@%\/?]/;
  const FRAG_KEEP = /[A-Za-z0-9\-._~!$&'()*+,;=:@%\/?#]/;
  const USER_KEEP = /[A-Za-z0-9\-._~!$&'()*+,;=%]/;

  function normalizePath(path, special) {
    if (special) path = path.replace(/\\/g, "/");
    const segs = path.split("/");
    const out = [];
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i];
      const last = i === segs.length - 1;
      if (s === "." || s.toLowerCase() === "%2e") {
        if (last) out.push("");
      } else if (s === ".." || /^(\.|%2e)(\.|%2e)$/i.test(s)) {
        if (out.length > 1) out.pop();
        if (last) out.push("");
      } else out.push(s);
    }
    let p = out.join("/");
    if (!p.startsWith("/")) p = `/${p}`;
    return p;
  }

  function parseAbsolute(input) {
    const m = /^([A-Za-z][A-Za-z0-9+.\-]*):(.*)$/s.exec(input);
    if (!m) return null;
    const protocol = `${m[1].toLowerCase()}:`;
    let rest = m[2];
    const special = protocol in SPECIAL;
    const r = { protocol, username: "", password: "", hostname: "", port: "", pathname: "", search: "", hash: "", opaque: false };
    const hashAt = rest.indexOf("#");
    if (hashAt >= 0) {
      r.hash = rest.slice(hashAt);
      rest = rest.slice(0, hashAt);
    }
    const qAt = rest.indexOf("?");
    if (qAt >= 0) {
      r.search = rest.slice(qAt);
      rest = rest.slice(0, qAt);
    }
    if (special) rest = rest.replace(/\\/g, "/");
    if (rest.startsWith("//") || (special && protocol !== "file:")) {
      rest = rest.replace(/^\/*/, "");
      const slash = rest.indexOf("/");
      let auth = slash < 0 ? rest : rest.slice(0, slash);
      rest = slash < 0 ? "" : rest.slice(slash);
      const at = auth.lastIndexOf("@");
      if (at >= 0) {
        const cred = auth.slice(0, at);
        auth = auth.slice(at + 1);
        const colon = cred.indexOf(":");
        r.username = encodeSet(colon < 0 ? cred : cred.slice(0, colon), USER_KEEP);
        r.password = colon < 0 ? "" : encodeSet(cred.slice(colon + 1), USER_KEEP);
      }
      const pm = /^(\[[^\]]*\]|[^:]*)(?::(\d*))?$/.exec(auth);
      if (!pm) throw new TypeError(`Invalid URL: ${input}`);
      let host = pm[1];
      if (/[\s<>^|%]/.test(host) && !host.startsWith("[")) throw new TypeError(`Invalid URL: ${input}`);
      if (special) {
        host = percentDecode(host).toLowerCase();
        if (!host && protocol !== "file:") throw new TypeError(`Invalid URL: ${input}`);
      }
      r.hostname = host;
      const port = pm[2] || "";
      if (port && Number(port) > 65535) throw new TypeError(`Invalid URL: ${input}`);
      r.port = port && SPECIAL[protocol] === String(Number(port)) ? "" : port ? String(Number(port)) : "";
      r.pathname = special ? normalizePath(rest || "/", true) : rest ? normalizePath(rest, false) : "";
    } else if (protocol === "file:") {
      r.pathname = normalizePath(rest || "/", true);
    } else {
      r.opaque = !rest.startsWith("/");
      r.pathname = r.opaque ? rest : normalizePath(rest, false);
    }
    r.pathname = r.opaque ? r.pathname : encodeSet(r.pathname, PATH_KEEP);
    if (r.search === "?") r.search = "";
    else if (r.search) r.search = encodeSet(r.search, QUERY_KEEP);
    if (r.hash === "#") r.hash = "";
    else if (r.hash) r.hash = encodeSet(r.hash, FRAG_KEEP);
    return r;
  }

  function resolve(input, base) {
    const abs = /^[A-Za-z][A-Za-z0-9+.\-]*:/.test(input) ? parseAbsolute(input) : null;
    if (abs && !(abs.protocol === base?._r?.protocol && abs.protocol in SPECIAL && !/^[A-Za-z][A-Za-z0-9+.\-]*:[\/\\]/.test(input))) return abs;
    if (!base) throw new TypeError(`Invalid URL: ${input}`);
    const b = base._r;
    if (b.opaque) {
      if (input.startsWith("#")) return { ...b, hash: input === "#" ? "" : encodeSet(input, FRAG_KEEP) };
      throw new TypeError(`Invalid URL: ${input}`);
    }
    let rel = abs ? input.replace(/^[A-Za-z][A-Za-z0-9+.\-]*:/, "") : input;
    if (b.protocol in SPECIAL) rel = rel.replace(/\\/g, "/");
    if (rel.startsWith("//")) return parseAbsolute(`${b.protocol}${rel}`);
    const r = { ...b, hash: "" };
    let hash = "";
    const h = rel.indexOf("#");
    if (h >= 0) {
      hash = rel.slice(h);
      rel = rel.slice(0, h);
    }
    let search = null;
    const q = rel.indexOf("?");
    if (q >= 0) {
      search = rel.slice(q);
      rel = rel.slice(0, q);
    }
    if (rel.startsWith("/")) r.pathname = normalizePath(rel, b.protocol in SPECIAL);
    else if (rel) r.pathname = normalizePath(b.pathname.replace(/[^\/]*$/, "") + rel, b.protocol in SPECIAL);
    r.pathname = encodeSet(r.pathname, PATH_KEEP);
    if (search !== null) r.search = search === "?" ? "" : encodeSet(search, QUERY_KEEP);
    else if (rel) r.search = "";
    r.hash = hash === "#" || !hash ? "" : encodeSet(hash, FRAG_KEEP);
    return r;
  }

  class URL {
    constructor(url, base) {
      const b = base === undefined ? undefined : base instanceof URL ? base : new URL(String(base));
      define(this, "_r", resolve(String(url).trim(), b));
      define(this, "_params", new URLSearchParams());
      URLSearchParams._link(this._params, this, this._r.search);
    }
    static canParse(url, base) {
      try {
        new URL(url, base);
        return true;
      } catch {
        return false;
      }
    }
    static parse(url, base) {
      try {
        return new URL(url, base);
      } catch {
        return null;
      }
    }
    _setSearchFromParams(s) {
      this._r.search = s ? `?${s}` : "";
    }
    get protocol() {
      return this._r.protocol;
    }
    set protocol(v) {
      const p = `${String(v).replace(/:.*$/, "").toLowerCase()}:`;
      if ((p in SPECIAL) === (this._r.protocol in SPECIAL)) this._r.protocol = p;
    }
    get username() {
      return this._r.username;
    }
    set username(v) {
      this._r.username = encodeSet(String(v), USER_KEEP);
    }
    get password() {
      return this._r.password;
    }
    set password(v) {
      this._r.password = encodeSet(String(v), USER_KEEP);
    }
    get hostname() {
      return this._r.hostname;
    }
    set hostname(v) {
      if (!this._r.opaque) this._r.hostname = String(v).toLowerCase();
    }
    get port() {
      return this._r.port;
    }
    set port(v) {
      const s = String(v);
      if (s === "") this._r.port = "";
      else if (/^\d+$/.test(s) && Number(s) <= 65535) this._r.port = SPECIAL[this._r.protocol] === String(Number(s)) ? "" : String(Number(s));
    }
    get host() {
      return this._r.port ? `${this._r.hostname}:${this._r.port}` : this._r.hostname;
    }
    set host(v) {
      const m = /^(\[[^\]]*\]|[^:\/?#]*)(?::(\d*))?/.exec(String(v));
      if (m && m[1]) {
        this.hostname = m[1];
        if (m[2] !== undefined) this.port = m[2];
      }
    }
    get origin() {
      const r = this._r;
      return r.protocol in SPECIAL && r.protocol !== "file:" ? `${r.protocol}//${this.host}` : "null";
    }
    get pathname() {
      return this._r.pathname;
    }
    set pathname(v) {
      if (!this._r.opaque) this._r.pathname = encodeSet(normalizePath(String(v), this._r.protocol in SPECIAL), PATH_KEEP);
    }
    get search() {
      return this._r.search;
    }
    set search(v) {
      let s = String(v);
      if (s.startsWith("?")) s = s.slice(1);
      this._r.search = s ? `?${encodeSet(s, QUERY_KEEP)}` : "";
      URLSearchParams._reparse(this._params, this._r.search);
    }
    get searchParams() {
      return this._params;
    }
    get hash() {
      return this._r.hash;
    }
    set hash(v) {
      let s = String(v);
      if (s.startsWith("#")) s = s.slice(1);
      this._r.hash = s ? `#${encodeSet(s, FRAG_KEEP)}` : "";
    }
    get href() {
      const r = this._r;
      if (r.opaque) return `${r.protocol}${r.pathname}${r.search}${r.hash}`;
      const cred = r.username || r.password ? `${r.username}${r.password ? `:${r.password}` : ""}@` : "";
      const authority = r.hostname || r.protocol in SPECIAL || cred ? `//${cred}${this.host}` : "";
      return `${r.protocol}${authority}${r.pathname}${r.search}${r.hash}`;
    }
    set href(v) {
      const r = resolve(String(v), undefined);
      this._r = r;
      URLSearchParams._reparse(this._params, r.search);
    }
    toString() {
      return this.href;
    }
    toJSON() {
      return this.href;
    }
    get [Symbol.toStringTag]() {
      return "URL";
    }
  }
  define(globalThis, "URL", URL);
  define(globalThis, "URLSearchParams", URLSearchParams);

  // ---------------------------------------------------------------- Headers

  const TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
  const normValue = (v) => String(v).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, "");
  class Headers {
    #map = new Map(); // lower-case name -> values
    #guard = "none";
    constructor(init) {
      if (init instanceof Headers) for (const [k, v] of init.#map) this.#map.set(k, [...v]);
      else if (init && typeof init === "object") {
        if (typeof init[Symbol.iterator] === "function") {
          for (const pair of init) {
            const p = [...pair];
            if (p.length !== 2) throw new TypeError("each header pair must have exactly two items");
            this.append(p[0], p[1]);
          }
        } else for (const k of Object.keys(init)) this.append(k, init[k]);
      }
    }
    static _immutable(h) {
      h.#guard = "immutable";
      return h;
    }
    #check(name) {
      if (this.#guard === "immutable") throw new TypeError("these headers are immutable");
      const n = String(name);
      if (!TOKEN.test(n)) throw new TypeError(`invalid header name ${JSON.stringify(n)}`);
      return n.toLowerCase();
    }
    append(name, value) {
      const n = this.#check(name);
      const v = normValue(value);
      if (/[\0\r\n]/.test(v)) throw new TypeError(`invalid value for header ${n}`);
      const cur = this.#map.get(n);
      if (cur) cur.push(v);
      else this.#map.set(n, [v]);
    }
    set(name, value) {
      const n = this.#check(name);
      const v = normValue(value);
      if (/[\0\r\n]/.test(v)) throw new TypeError(`invalid value for header ${n}`);
      this.#map.set(n, [v]);
    }
    delete(name) {
      this.#map.delete(this.#check(name));
    }
    get(name) {
      const v = this.#map.get(String(name).toLowerCase());
      if (!v) return null;
      return v.join(", ");
    }
    getSetCookie() {
      return [...(this.#map.get("set-cookie") || [])];
    }
    has(name) {
      return this.#map.has(String(name).toLowerCase());
    }
    *entries() {
      for (const k of [...this.#map.keys()].sort()) {
        if (k === "set-cookie") for (const v of this.#map.get(k)) yield [k, v];
        else yield [k, this.#map.get(k).join(", ")];
      }
    }
    *keys() {
      for (const [k] of this.entries()) yield k;
    }
    *values() {
      for (const [, v] of this.entries()) yield v;
    }
    forEach(fn, thisArg) {
      for (const [k, v] of this.entries()) fn.call(thisArg, v, k, this);
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    get [Symbol.toStringTag]() {
      return "Headers";
    }
  }
  define(globalThis, "Headers", Headers);
  const headersObject = (h) => {
    const o = {};
    for (const [k, v] of h) o[k] = o[k] === undefined ? v : `${o[k]}, ${v}`;
    return o;
  };

  // ---------------------------------------------------------- Blob, FormData

  class Blob {
    #bytes;
    #type;
    constructor(parts = [], options = {}) {
      const chunks = [];
      for (const p of parts) chunks.push(p instanceof Blob ? p.#bytes : isBytes(p) ? toU8(p) : utf8(p));
      const total = chunks.reduce((n, c) => n + c.length, 0);
      this.#bytes = new Uint8Array(total);
      let o = 0;
      for (const c of chunks) {
        this.#bytes.set(c, o);
        o += c.length;
      }
      this.#type = options.type ? String(options.type).toLowerCase() : "";
    }
    get size() {
      return this.#bytes.length;
    }
    get type() {
      return this.#type;
    }
    static _bytes(b) {
      return b.#bytes;
    }
    async text() {
      return fromUtf8(this.#bytes);
    }
    async arrayBuffer() {
      return copyBuffer(this.#bytes);
    }
    async bytes() {
      return this.#bytes.slice();
    }
    slice(start = 0, end = this.size, type = "") {
      return new Blob([this.#bytes.slice(start, end)], { type });
    }
  }
  define(globalThis, "Blob", Blob);

  // -------------------------------------------------------------- Body mixin

  // Turns a BodyInit into bytes and a default content type.
  function extractBody(body) {
    if (body === undefined || body === null) return [null, null];
    if (typeof body === "string") return [utf8(body), "text/plain;charset=UTF-8"];
    if (body instanceof URLSearchParams) return [utf8(body.toString()), "application/x-www-form-urlencoded;charset=UTF-8"];
    if (body instanceof Blob) return [Blob._bytes(body).slice(), body.type || null];
    if (isBytes(body)) return [toU8(body).slice(), null];
    if (typeof body === "object" && typeof body.getReader === "function") throw new TypeError("streaming bodies are not supported");
    return [utf8(String(body)), "text/plain;charset=UTF-8"];
  }

  class Body {
    #bytes;
    #used = false;
    _initBody(bytes) {
      this.#bytes = bytes;
    }
    get bodyUsed() {
      return this.#used;
    }
    get body() {
      return this.#bytes === null ? null : this.#bytes;
    }
    _peek() {
      return this.#bytes;
    }
    #take() {
      if (this.#used) throw new TypeError("the body has already been read");
      this.#used = true;
      return this.#bytes || new Uint8Array(0);
    }
    async arrayBuffer() {
      return copyBuffer(this.#take());
    }
    async bytes() {
      return this.#take().slice();
    }
    async text() {
      return fromUtf8(this.#take());
    }
    async json() {
      return JSON.parse(fromUtf8(this.#take()));
    }
    async blob() {
      return new Blob([this.#take()], { type: this.headers.get("content-type") || "" });
    }
    async formData() {
      const ct = this.headers.get("content-type") || "";
      if (!ct.startsWith("application/x-www-form-urlencoded")) throw new TypeError("only application/x-www-form-urlencoded bodies can be read as form data");
      return new URLSearchParams(fromUtf8(this.#take()));
    }
  }

  const METHODS = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT", "PATCH"];
  class Request extends Body {
    #url;
    #method;
    #headers;
    #redirect;
    #signal;
    constructor(input, init = {}) {
      super();
      let bytes = null;
      if (input instanceof Request) {
        this.#url = input.url;
        this.#method = input.method;
        this.#headers = new Headers(input.headers);
        this.#redirect = input.redirect;
        bytes = input._peek();
      } else {
        this.#url = new URL(String(input)).href;
        this.#method = "GET";
        this.#headers = new Headers();
        this.#redirect = "follow";
      }
      if (init.method !== undefined) {
        const m = String(init.method);
        if (!TOKEN.test(m)) throw new TypeError(`invalid method ${m}`);
        this.#method = METHODS.includes(m.toUpperCase()) ? m.toUpperCase() : m;
      }
      if (init.headers !== undefined) this.#headers = new Headers(init.headers);
      if (init.redirect !== undefined) this.#redirect = String(init.redirect);
      this.#signal = init.signal || null;
      if (init.body !== undefined && init.body !== null) {
        if (this.#method === "GET" || this.#method === "HEAD") throw new TypeError("a GET or HEAD request cannot have a body");
        const [b, type] = extractBody(init.body);
        bytes = b;
        if (type && !this.#headers.has("content-type")) this.#headers.set("content-type", type);
      }
      this._initBody(bytes);
    }
    get url() {
      return this.#url;
    }
    get method() {
      return this.#method;
    }
    get headers() {
      return this.#headers;
    }
    get redirect() {
      return this.#redirect;
    }
    get signal() {
      return this.#signal;
    }
    clone() {
      if (this.bodyUsed) throw new TypeError("the body has already been read");
      return new Request(this);
    }
    get [Symbol.toStringTag]() {
      return "Request";
    }
  }

  const NULL_BODY = [101, 103, 204, 205, 304];
  const REDIRECTS = [301, 302, 303, 307, 308];
  class Response extends Body {
    #status;
    #statusText;
    #headers;
    #type;
    #url = "";
    #redirected = false;
    constructor(body = null, init = {}) {
      super();
      const status = init.status === undefined ? 200 : Number(init.status);
      if (!Number.isInteger(status) || status < 200 || status > 599) throw new RangeError(`invalid status ${init.status}`);
      this.#status = status;
      this.#statusText = init.statusText === undefined ? "" : String(init.statusText);
      this.#headers = new Headers(init.headers);
      this.#type = "default";
      const [bytes, type] = extractBody(body);
      if (bytes !== null && NULL_BODY.includes(status)) throw new TypeError(`a ${status} response cannot have a body`);
      if (type && !this.#headers.has("content-type")) this.#headers.set("content-type", type);
      this._initBody(bytes);
    }
    static json(data, init = {}) {
      const text = JSON.stringify(data);
      if (text === undefined) throw new TypeError("the value cannot be serialised as JSON");
      const headers = new Headers(init.headers);
      if (!headers.has("content-type")) headers.set("content-type", "application/json");
      return new Response(text, { ...init, headers });
    }
    static redirect(url, status = 302) {
      if (!REDIRECTS.includes(status)) throw new RangeError(`invalid redirect status ${status}`);
      return new Response(null, { status, headers: { location: new URL(String(url)).href } });
    }
    static error() {
      const r = new Response(null, { status: 200 });
      r.#type = "error";
      r.#status = 0;
      return r;
    }
    static _fromFetch(status, headers, bytes, url, redirected) {
      const r = new Response(null, { status: status < 200 ? 200 : status, headers });
      r.#status = status;
      r._initBody(NULL_BODY.includes(status) ? null : bytes);
      r.#url = url;
      r.#redirected = redirected;
      r.#type = "basic";
      Headers._immutable(r.#headers);
      return r;
    }
    get status() {
      return this.#status;
    }
    get statusText() {
      return this.#statusText;
    }
    get ok() {
      return this.#status >= 200 && this.#status <= 299;
    }
    get headers() {
      return this.#headers;
    }
    get type() {
      return this.#type;
    }
    get url() {
      return this.#url;
    }
    get redirected() {
      return this.#redirected;
    }
    clone() {
      if (this.bodyUsed) throw new TypeError("the body has already been read");
      const b = this._peek();
      const r = new Response(b === null ? null : b.slice(), { status: this.#status < 200 ? 200 : this.#status, statusText: this.#statusText, headers: this.#headers });
      r.#status = this.#status;
      r.#url = this.#url;
      r.#type = this.#type;
      return r;
    }
    get [Symbol.toStringTag]() {
      return "Response";
    }
  }
  define(globalThis, "Request", Request);
  define(globalThis, "Response", Response);

  // ------------------------------------------------------------------ fetch

  async function fetch(input, init = {}) {
    const req = input instanceof Request && Object.keys(init).length === 0 ? input : new Request(input, init);
    if (req.signal && req.signal.aborted) throw new DOMException("the request was aborted", "AbortError");
    const u = new URL(req.url);
    if (u.protocol !== "http:" && u.protocol !== "https:") throw new TypeError(`fetch supports only http and https URLs, not ${u.protocol}`);
    const bytes = req._peek();
    const arg = { url: req.url, method: req.method, headers: headersObject(req.headers) };
    if (bytes && bytes.length) arg.body_base64 = N.b64encode(bytes);
    let res;
    try {
      res = hostJson("fetch", arg);
    } catch (e) {
      if (e instanceof ForgeBaseError) {
        const err = new TypeError(`fetch failed: ${e.message}`);
        err.code = e.code;
        throw err;
      }
      throw e;
    }
    const body = res.body_base64 !== undefined ? N.b64decode(res.body_base64) : utf8(res.body || "");
    return Response._fromFetch(res.status, res.headers || {}, body, res.url || req.url, (res.url || req.url) !== req.url);
  }
  define(globalThis, "fetch", fetch);

  // ----------------------------------------------------------------- crypto

  const DIGESTS = { "SHA-1": "sha-1", "SHA-256": "sha-256", "SHA-384": "sha-384", "SHA-512": "sha-512" };
  const algName = (a) => {
    const n = (typeof a === "string" ? a : a && a.name ? a.name : "").toUpperCase();
    if (!(n in DIGESTS)) throw new DOMException(`unsupported algorithm ${n || a}`, "NotSupportedError");
    return n;
  };
  class CryptoKey {
    constructor(algorithm, extractable, usages, raw) {
      define(this, "_raw", raw);
      this.type = "secret";
      this.extractable = !!extractable;
      this.algorithm = algorithm;
      this.usages = [...usages];
    }
  }
  const bytesOf = (data) => {
    if (!isBytes(data)) throw new TypeError("expected an ArrayBuffer or a typed array");
    return toU8(data);
  };
  const subtle = {
    async digest(algorithm, data) {
      const name = algName(algorithm);
      const r = hostJson("crypto.digest", { algorithm: DIGESTS[name], data_base64: N.b64encode(bytesOf(data)) });
      return copyBuffer(N.b64decode(r.base64));
    },
    async importKey(format, keyData, algorithm, extractable, usages) {
      if (format !== "raw") throw new DOMException("only raw keys are supported", "NotSupportedError");
      const name = (typeof algorithm === "string" ? algorithm : algorithm.name || "").toUpperCase();
      if (name !== "HMAC") throw new DOMException("only HMAC keys are supported", "NotSupportedError");
      const hash = algName(algorithm.hash || "SHA-256");
      const raw = bytesOf(keyData).slice();
      return new CryptoKey({ name: "HMAC", hash: { name: hash }, length: raw.length * 8 }, extractable, usages || [], raw);
    },
    async exportKey(format, key) {
      if (format !== "raw" || !key.extractable) throw new DOMException("the key cannot be exported", "InvalidAccessError");
      return copyBuffer(key._raw);
    },
    async sign(algorithm, key, data) {
      if (!(key instanceof CryptoKey) || key.algorithm.name !== "HMAC") throw new DOMException("expected an HMAC key", "InvalidAccessError");
      const r = hostJson("crypto.hmac", {
        algorithm: DIGESTS[key.algorithm.hash.name],
        key_base64: N.b64encode(key._raw),
        data_base64: N.b64encode(bytesOf(data)),
      });
      return copyBuffer(N.b64decode(r.base64));
    },
    async verify(algorithm, key, signature, data) {
      const expected = new Uint8Array(await subtle.sign(algorithm, key, data));
      const got = bytesOf(signature);
      if (got.length !== expected.length) return false;
      let diff = 0;
      for (let i = 0; i < got.length; i++) diff |= got[i] ^ expected[i];
      return diff === 0;
    },
  };
  const crypto = {
    subtle,
    getRandomValues(arr) {
      if (!ArrayBuffer.isView(arr) || arr instanceof Float32Array || arr instanceof Float64Array || arr instanceof DataView)
        throw new TypeError("getRandomValues takes an integer typed array");
      if (arr.byteLength > 65536) throw new DOMException("at most 65536 bytes", "QuotaExceededError");
      new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength).set(N.random(arr.byteLength));
      return arr;
    },
    randomUUID() {
      const b = N.random(16);
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
      return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
    },
  };
  define(globalThis, "crypto", crypto);
  define(globalThis, "CryptoKey", CryptoKey);

  // ---------------------------------------------------------- AbortController

  class AbortSignal {
    constructor() {
      this.aborted = false;
      this.reason = undefined;
      this.onabort = null;
      define(this, "_listeners", []);
    }
    addEventListener(type, fn) {
      if (type === "abort") this._listeners.push(fn);
    }
    removeEventListener(type, fn) {
      const i = this._listeners.indexOf(fn);
      if (i >= 0) this._listeners.splice(i, 1);
    }
    throwIfAborted() {
      if (this.aborted) throw this.reason;
    }
    static abort(reason) {
      const c = new AbortController();
      c.abort(reason);
      return c.signal;
    }
    static timeout(ms) {
      const c = new AbortController();
      setTimeout(() => c.abort(new DOMException("the operation timed out", "TimeoutError")), ms);
      return c.signal;
    }
  }
  class AbortController {
    constructor() {
      this.signal = new AbortSignal();
    }
    abort(reason = new DOMException("the operation was aborted", "AbortError")) {
      const s = this.signal;
      if (s.aborted) return;
      s.aborted = true;
      s.reason = reason;
      const ev = { type: "abort", target: s };
      if (typeof s.onabort === "function") s.onabort(ev);
      for (const fn of [...s._listeners]) fn(ev);
    }
  }
  define(globalThis, "AbortController", AbortController);
  define(globalThis, "AbortSignal", AbortSignal);

  // -------------------------------------------------------- forgebase module

  let handler = null;
  function serve(fn) {
    if (typeof fn === "object" && fn !== null && typeof fn.fetch === "function") fn = fn.fetch.bind(fn);
    if (typeof fn !== "function") throw new TypeError("serve() takes a handler function (req, ctx) => Response");
    if (handler) throw new Error("serve() may be called only once");
    handler = fn;
  }

  const envGet = (name) => hostJson("env.get", { name: String(name) }).value ?? undefined;
  define(globalThis, "Deno", Object.freeze({ env: Object.freeze({ get: envGet }) }));

  const strValue = (v) => (typeof v === "string" ? v : typeof v === "number" || typeof v === "bigint" || typeof v === "boolean" ? String(v) : JSON.stringify(v));
  const kvApi = (as, namespace) => {
    const kv = (cmd) => hostJson("kv", { as, namespace, cmd }).value;
    const api = {
      get: async (key) => kv(["GET", String(key)]),
      set: async (key, value, opts = {}) => {
        const cmd = ["SET", String(key), strValue(value)];
        if (opts.ttlMs !== undefined) cmd.push("PX", String(Math.max(1, Math.trunc(opts.ttlMs))));
        if (opts.nx) cmd.push("NX");
        if (opts.xx) cmd.push("XX");
        return kv(cmd) === "OK";
      },
      del: async (...keys) => kv(["DEL", ...keys.flat().map(String)]),
      incr: async (key, by = 1) => kv(["INCRBY", String(key), String(Math.trunc(by))]),
      decr: async (key, by = 1) => kv(["DECRBY", String(key), String(Math.trunc(by))]),
      expire: async (key, ttlMs) => kv(["PEXPIRE", String(key), String(Math.max(1, Math.trunc(ttlMs)))]) === 1,
      ttl: async (key) => kv(["PTTL", String(key)]),
      exists: async (key) => kv(["EXISTS", String(key)]) > 0,
      keys: async (prefix = "") => {
        const glob = `${String(prefix).replace(/[*?[\]\\]/g, "\\$&")}*`;
        const out = [];
        let cursor = "0";
        do {
          const [next, batch] = kv(["SCAN", cursor, "MATCH", glob, "COUNT", "500"]);
          out.push(...batch);
          cursor = String(next);
        } while (cursor !== "0" && out.length < 100000);
        return out.sort();
      },
      hget: async (key, field) => kv(["HGET", String(key), String(field)]),
      hset: async (key, field, value) => {
        const pairs = typeof field === "object" && field !== null ? Object.entries(field) : [[field, value]];
        return kv(["HSET", String(key), ...pairs.flatMap(([f, v]) => [String(f), strValue(v)])]);
      },
      hdel: async (key, ...fields) => kv(["HDEL", String(key), ...fields.flat().map(String)]),
      hgetall: async (key) => {
        const flat = kv(["HGETALL", String(key)]) || [];
        const o = {};
        for (let i = 0; i + 1 < flat.length; i += 2) o[flat[i]] = flat[i + 1];
        return o;
      },
      command: async (...cmd) => kv(cmd.flat().map(String)),
    };
    if (namespace === null) api.namespace = (ns) => kvApi(as, String(ns));
    return Object.freeze(api);
  };

  const listArgs = (o = {}) => {
    const a = {};
    if (o.filter !== undefined) a.filter = String(o.filter);
    if (o.params !== undefined) a.params = o.params;
    if (o.sort !== undefined) a.sort = String(o.sort);
    if (o.page !== undefined) a.page = o.page;
    if (o.perPage !== undefined) a.per_page = o.perPage;
    if (o.cursor !== undefined) a.cursor = o.cursor;
    return a;
  };

  const services = (as) => {
    const db = Object.freeze({
      from(collection) {
        const c = String(collection);
        return Object.freeze({
          get: (id) => call("db.get", { as, collection: c, id: String(id) }),
          list: (opts) => call("db.list", { as, collection: c, ...listArgs(opts) }),
          create: (data) => call("db.create", { as, collection: c, data }),
          update: (id, data, opts = {}) => {
            const a = { as, collection: c, id: String(id), data };
            if (opts.expectedVersion !== undefined) a.expected_version = opts.expectedVersion;
            return call("db.update", a);
          },
          delete: (id) => call("db.delete", { as, collection: c, id: String(id) }).then(() => undefined),
        });
      },
    });
    const vector = Object.freeze({
      from(collection) {
        const c = String(collection);
        return Object.freeze({
          search: (q = {}) => {
            const a = { as, collection: c, k: q.k ?? 10 };
            for (const [from, to] of [["vector", "vector"], ["text", "text"], ["filter", "filter"], ["params", "params"], ["field", "field"], ["includeVectors", "include_vectors"], ["includeText", "include_text"]])
              if (q[from] !== undefined) a[to] = q[from];
            return call("vector.search", a);
          },
          upsert: (items) => call("vector.upsert", { as, collection: c, items: Array.isArray(items) ? items : [items] }),
          get: (id) => call("vector.get", { as, collection: c, id: String(id) }),
          delete: (ids) => call("vector.delete", { as, collection: c, ids: (Array.isArray(ids) ? ids : [ids]).map(String) }),
        });
      },
    });
    const storage = Object.freeze({
      from(bucket) {
        const b = String(bucket);
        return Object.freeze({
          get: async (key) => {
            let r;
            try {
              r = hostJson("storage.get", { as, bucket: b, key: String(key) });
            } catch (e) {
              if (e instanceof ForgeBaseError && e.code === "not_found") return null;
              throw e;
            }
            const bytes = N.b64decode(r.body_base64);
            const info = r.info;
            return Object.freeze({
              ...info,
              key: info.key,
              size: bytes.length,
              contentType: info.content_type,
              bytes: async () => bytes.slice(),
              arrayBuffer: async () => copyBuffer(bytes),
              text: async () => fromUtf8(bytes),
              json: async () => JSON.parse(fromUtf8(bytes)),
            });
          },
          put: (key, body, opts = {}) => {
            const [bytes, type] = extractBody(body);
            const a = { as, bucket: b, key: String(key), body_base64: N.b64encode(bytes || new Uint8Array(0)) };
            if (opts.contentType !== undefined || type) a.content_type = opts.contentType !== undefined ? String(opts.contentType) : type.split(";")[0];
            if (opts.metadata !== undefined) a.metadata = opts.metadata;
            return call("storage.put", a);
          },
          list: (prefix = "", opts = {}) => {
            const a = { as, bucket: b, prefix: String(prefix) };
            if (opts.limit !== undefined) a.limit = opts.limit;
            if (opts.cursor !== undefined) a.cursor = opts.cursor;
            if (opts.delimiter !== undefined) a.delimiter = opts.delimiter;
            return call("storage.list", a);
          },
          delete: (key) => call("storage.delete", { as, bucket: b, key: String(key) }),
          signedUrl: (key, opts = {}) => {
            const a = { as, bucket: b, key: String(key) };
            if (opts.expiresIn !== undefined) a.expires_in = opts.expiresIn;
            if (opts.method !== undefined) a.method = String(opts.method);
            if (opts.downloadAs !== undefined) a.download_as = String(opts.downloadAs);
            if (opts.contentType !== undefined) a.content_type = String(opts.contentType);
            return call("storage.sign", a);
          },
        });
      },
    });
    const queues = Object.freeze({
      publish: (topic, message, opts = {}) => {
        const a = { as, topic: String(topic), data: message === undefined ? null : message };
        if (opts.key !== undefined) a.ordering_key = String(opts.key);
        if (opts.attributes !== undefined) a.attributes = opts.attributes;
        if (opts.dedupId !== undefined) a.dedup_id = String(opts.dedupId);
        return call("queues.publish", a);
      },
    });
    const jobs = Object.freeze({
      enqueue: (kind, payload, opts = {}) => {
        const a = { as, kind: String(kind), payload: payload === undefined ? null : payload };
        const map = { runAt: "run_at", delayMs: "delay_ms", maxAttempts: "max_attempts", queue: "queue", priority: "priority", idempotencyKey: "idempotency_key", timeoutMs: "timeout_ms" };
        for (const [k, v] of Object.entries(map)) if (opts[k] !== undefined) a[v] = k === "runAt" && opts[k] instanceof Date ? opts[k].toISOString() : opts[k];
        return call("jobs.enqueue", a);
      },
    });
    const realtime = Object.freeze({
      broadcast: (channel, event, payload) =>
        call("realtime.broadcast", { as, channel: String(channel), event: String(event), payload: payload === undefined ? null : payload }).then(() => undefined),
    });
    return Object.freeze({ db, vector, kv: kvApi(as, null), storage, queues, jobs, realtime });
  };

  const verify = (token) => call("auth.verify", { token: String(token) });

  function makeCtx(input) {
    const caller = services("caller");
    const a = input.auth;
    const auth =
      a && a.type !== "anonymous"
        ? Object.freeze({
            id: a.id ?? null,
            type: a.type,
            collection: a.collection ?? null,
            roles: Object.freeze([...(a.roles || [])]),
            permissions: Object.freeze([...(a.permissions || [])]),
            realm: a.realm ?? null,
            org: a.org ?? null,
            tenant: a.tenant ?? null,
            verify,
          })
        : null;
    return Object.freeze({
      auth,
      verifyToken: verify,
      env: Object.freeze({ get: envGet }),
      ...caller,
      admin: services("admin"),
      trigger: input.trigger,
      event: input.trigger === "http" ? null : input,
      requestId: input.request_id ?? null,
      function: Object.freeze({ name: input.function?.name ?? null, version: input.function?.version ?? null }),
      waitUntil: (p) => void Promise.resolve(p).catch((e) => console.error("waitUntil:", e)),
    });
  }

  const MAX_BODY = 4 * 1024 * 1024;
  async function run(raw) {
    const input = JSON.parse(raw);
    if (!handler) throw new Error("the function did not register a handler: call serve(handler) from \"forgebase\"");
    const ctx = makeCtx(input);
    let req;
    if (input.trigger === "http") {
      const headers = new Headers();
      for (const [k, v] of Object.entries(input.headers || {})) {
        try {
          headers.append(k, v);
        } catch {}
      }
      const body = input.body_base64 ? N.b64decode(input.body_base64) : null;
      const method = String(input.method || "GET").toUpperCase();
      req = new Request(input.url, { method, headers, body: method === "GET" || method === "HEAD" ? null : body });
    } else {
      req = new Request(`http://function.invalid/${input.trigger}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(input),
      });
    }
    const result = await handler(req, ctx);
    if (input.trigger === "http") {
      if (result === undefined || result === null) return JSON.stringify({ status: 204 });
      const res = result instanceof Response ? result : Response.json(result);
      if (res.type === "error") throw new TypeError("the handler returned Response.error()");
      const bytes = res._peek();
      if (bytes && bytes.length > MAX_BODY) throw new RangeError(`the response body is larger than ${MAX_BODY} bytes`);
      const out = { status: res.status, headers: headersObject(res.headers) };
      if (bytes && bytes.length) out.body_base64 = N.b64encode(bytes);
      return JSON.stringify(out);
    }
    if (result === undefined || result === null) return "";
    if (result instanceof Response) return fromUtf8(result._peek() || new Uint8Array(0));
    const text = JSON.stringify(result);
    return text === undefined ? "" : text;
  }

  globalThis[Symbol.for("forgebase.module")] = Object.freeze({ serve, ForgeBaseError });
  return { begin, run, tick };
})();
