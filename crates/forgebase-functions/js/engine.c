/*
 * ForgeBase JavaScript engine: QuickJS (quickjs-ng) built for wasm32-wasip1.
 *
 * The module is embedded in the ForgeBase binary and runs in the same
 * Wasmtime sandbox as WebAssembly functions (ADR-0017, ADR-0027). One fresh
 * instance runs one execution: `handle` creates a runtime, evaluates the
 * prelude (Web globals and the `forgebase` module), evaluates the function's
 * JavaScript (read with `source_read`) as an ES module, then calls the
 * prelude's `run` with the execution input and drives the job queue until the
 * returned promise settles. The result string is passed to `output`.
 *
 * Imports (module "forgebase"):
 *   input_len() -> i32, input_read(ptr, len) -> i32      execution input (JSON)
 *   source_len() -> i32, source_read(ptr, len) -> i32    the function's JavaScript
 *   output(ptr, len) -> i32                              the execution output
 *   result_read(ptr, len) -> i32                         last call's result
 *   log(level, ptr, len) -> i32                          an execution log line
 *   call(op_ptr, op_len, arg_ptr, arg_len) -> i32        a host operation:
 *       n >= 0: success, n-byte result; -n: n-byte {"code","message"} error
 *
 * Exports: init() -> i32 (runtime, context and prelude; the host snapshots the
 * memory after it), handle() -> i32 (0 = success, 1 = the function failed;
 * output then holds the error message), and __stack_pointer for the snapshot.
 */

#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "quickjs.h"

#define IMPORT(name) __attribute__((import_module("forgebase"), import_name(name)))

IMPORT("input_len") int32_t fb_input_len(void);
IMPORT("input_read") int32_t fb_input_read(uint8_t *ptr, int32_t len);
IMPORT("source_len") int32_t fb_source_len(void);
IMPORT("source_read") int32_t fb_source_read(uint8_t *ptr, int32_t len);
IMPORT("output") int32_t fb_output(const uint8_t *ptr, int32_t len);
IMPORT("result_read") int32_t fb_result_read(uint8_t *ptr, int32_t len);
IMPORT("log") int32_t fb_log(int32_t level, const uint8_t *ptr, int32_t len);
IMPORT("call") int32_t fb_call(const uint8_t *op, int32_t op_len, const uint8_t *arg, int32_t arg_len);

/* The prelude (prelude.js), embedded at build time. */
static const char PRELUDE[] = {
#embed "prelude.js"
    , 0
};

/* Reads a whole host buffer (input or source). */
static uint8_t *read_all(int32_t (*len_fn)(void), int32_t (*read_fn)(uint8_t *, int32_t), size_t *out_len)
{
    int32_t n = len_fn();
    if (n < 0)
        return NULL;
    uint8_t *buf = malloc((size_t)n + 1);
    if (!buf)
        return NULL;
    if (n > 0 && read_fn(buf, n) != n) {
        free(buf);
        return NULL;
    }
    buf[n] = 0;
    *out_len = (size_t)n;
    return buf;
}

static JSValue throw_host_error(JSContext *ctx, const uint8_t *json, size_t len)
{
    /* The prelude turns {"code","message"} into a ForgeBaseError. */
    JSValue err = JS_NewError(ctx);
    JS_DefinePropertyValueStr(ctx, err, "message", JS_NewString(ctx, "host call failed"),
                              JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE);
    JS_DefinePropertyValueStr(ctx, err, "hostError", JS_NewStringLen(ctx, (const char *)json, len),
                              JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE);
    return JS_Throw(ctx, err);
}

/* native.call(op, arg) -> string */
static JSValue n_call(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    size_t op_len = 0, arg_len = 0;
    const char *op = JS_ToCStringLen(ctx, &op_len, argv[0]);
    if (!op)
        return JS_EXCEPTION;
    const char *arg = argc > 1 ? JS_ToCStringLen(ctx, &arg_len, argv[1]) : NULL;
    if (argc > 1 && !arg) {
        JS_FreeCString(ctx, op);
        return JS_EXCEPTION;
    }
    int32_t n = fb_call((const uint8_t *)op, (int32_t)op_len, (const uint8_t *)(arg ? arg : ""), (int32_t)arg_len);
    JS_FreeCString(ctx, op);
    if (arg)
        JS_FreeCString(ctx, arg);
    int32_t size = n < 0 ? -n : n;
    uint8_t *buf = malloc((size_t)size + 1);
    if (!buf)
        return JS_ThrowOutOfMemory(ctx);
    if (size > 0 && fb_result_read(buf, size) != size) {
        free(buf);
        return JS_ThrowInternalError(ctx, "cannot read the host result");
    }
    JSValue ret = n < 0 ? throw_host_error(ctx, buf, (size_t)size) : JS_NewStringLen(ctx, (const char *)buf, (size_t)size);
    free(buf);
    return ret;
}

/* native.log(level, message) */
static JSValue n_log(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    int32_t level = 1;
    if (JS_ToInt32(ctx, &level, argv[0]))
        return JS_EXCEPTION;
    size_t len = 0;
    const char *s = JS_ToCStringLen(ctx, &len, argv[1]);
    if (!s)
        return JS_EXCEPTION;
    fb_log(level, (const uint8_t *)s, (int32_t)len);
    JS_FreeCString(ctx, s);
    return JS_UNDEFINED;
}

/* Bytes of a Uint8Array, ArrayBuffer or other typed array / DataView. */
static uint8_t *bytes_of(JSContext *ctx, JSValueConst v, size_t *len)
{
    uint8_t *p = JS_GetUint8Array(ctx, len, v);
    if (p)
        return p;
    JS_FreeValue(ctx, JS_GetException(ctx));
    p = JS_GetArrayBuffer(ctx, len, v);
    if (p)
        return p;
    JS_FreeValue(ctx, JS_GetException(ctx));
    size_t off = 0, blen = 0, per = 0;
    JSValue buf = JS_GetTypedArrayBuffer(ctx, v, &off, &blen, &per);
    if (JS_IsException(buf)) {
        JS_FreeValue(ctx, JS_GetException(ctx));
        JS_ThrowTypeError(ctx, "expected an ArrayBuffer or a typed array");
        return NULL;
    }
    size_t total = 0;
    uint8_t *base = JS_GetArrayBuffer(ctx, &total, buf);
    JS_FreeValue(ctx, buf);
    if (!base)
        return NULL;
    *len = blen;
    return base + off;
}

/* native.utf8encode(string) -> Uint8Array (lone surrogates become U+FFFD) */
static JSValue n_utf8encode(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    size_t len = 0;
    const char *s = JS_ToCStringLen(ctx, &len, argv[0]);
    if (!s)
        return JS_EXCEPTION;
    JSValue out = JS_NewUint8ArrayCopy(ctx, (const uint8_t *)s, len);
    JS_FreeCString(ctx, s);
    return out;
}

/* Appends code point `c` to `out` as UTF-8; returns bytes written. */
static size_t put_utf8(uint8_t *out, uint32_t c)
{
    if (c < 0x80) {
        out[0] = (uint8_t)c;
        return 1;
    }
    if (c < 0x800) {
        out[0] = (uint8_t)(0xC0 | (c >> 6));
        out[1] = (uint8_t)(0x80 | (c & 0x3F));
        return 2;
    }
    if (c < 0x10000) {
        out[0] = (uint8_t)(0xE0 | (c >> 12));
        out[1] = (uint8_t)(0x80 | ((c >> 6) & 0x3F));
        out[2] = (uint8_t)(0x80 | (c & 0x3F));
        return 3;
    }
    out[0] = (uint8_t)(0xF0 | (c >> 18));
    out[1] = (uint8_t)(0x80 | ((c >> 12) & 0x3F));
    out[2] = (uint8_t)(0x80 | ((c >> 6) & 0x3F));
    out[3] = (uint8_t)(0x80 | (c & 0x3F));
    return 4;
}

/* native.utf8decode(bytes, fatal, ignoreBOM) -> string */
static JSValue n_utf8decode(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    size_t len = 0;
    const uint8_t *p = bytes_of(ctx, argv[0], &len);
    if (!p)
        return JS_EXCEPTION;
    int fatal = argc > 1 && JS_ToBool(ctx, argv[1]);
    int ignore_bom = argc > 2 && JS_ToBool(ctx, argv[2]);
    size_t i = 0;
    if (!ignore_bom && len >= 3 && p[0] == 0xEF && p[1] == 0xBB && p[2] == 0xBF)
        i = 3;
    /* Worst case: every byte becomes U+FFFD (3 bytes). */
    uint8_t *out = malloc(len * 3 + 1);
    if (!out)
        return JS_ThrowOutOfMemory(ctx);
    size_t o = 0;
    while (i < len) {
        uint8_t b = p[i];
        uint32_t c;
        size_t need;
        if (b < 0x80) {
            out[o++] = b;
            i++;
            continue;
        } else if (b >= 0xC2 && b <= 0xDF) {
            c = b & 0x1F;
            need = 1;
        } else if (b >= 0xE0 && b <= 0xEF) {
            c = b & 0x0F;
            need = 2;
        } else if (b >= 0xF0 && b <= 0xF4) {
            c = b & 0x07;
            need = 3;
        } else {
            goto bad;
        }
        {
            size_t k;
            uint32_t lo = 0x80, hi = 0xBF;
            if (b == 0xE0)
                lo = 0xA0;
            else if (b == 0xED)
                hi = 0x9F;
            else if (b == 0xF0)
                lo = 0x90;
            else if (b == 0xF4)
                hi = 0x8F;
            for (k = 1; k <= need; k++) {
                if (i + k >= len)
                    goto bad_at;
                uint8_t cb = p[i + k];
                if (k == 1 ? (cb < lo || cb > hi) : (cb < 0x80 || cb > 0xBF))
                    goto bad_at;
                c = (c << 6) | (cb & 0x3F);
            }
            o += put_utf8(out + o, c);
            i += need + 1;
            continue;
        bad_at:
            if (fatal)
                goto fail;
            o += put_utf8(out + o, 0xFFFD);
            i += k;
            continue;
        }
    bad:
        if (fatal)
            goto fail;
        o += put_utf8(out + o, 0xFFFD);
        i++;
    }
    {
        JSValue s = JS_NewStringLen(ctx, (const char *)out, o);
        free(out);
        return s;
    }
fail:
    free(out);
    return JS_ThrowTypeError(ctx, "The encoded data was not valid UTF-8");
}

static const char B64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/* native.b64encode(bytes) -> string */
static JSValue n_b64encode(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    size_t len = 0;
    const uint8_t *p = bytes_of(ctx, argv[0], &len);
    if (!p)
        return JS_EXCEPTION;
    size_t olen = (len + 2) / 3 * 4;
    char *out = malloc(olen + 1);
    if (!out)
        return JS_ThrowOutOfMemory(ctx);
    size_t i = 0, o = 0;
    while (i + 2 < len) {
        uint32_t v = ((uint32_t)p[i] << 16) | ((uint32_t)p[i + 1] << 8) | p[i + 2];
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
        out[o++] = B64[(v >> 6) & 63];
        out[o++] = B64[v & 63];
        i += 3;
    }
    if (i < len) {
        uint32_t v = (uint32_t)p[i] << 16;
        if (i + 1 < len)
            v |= (uint32_t)p[i + 1] << 8;
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
        out[o++] = i + 1 < len ? B64[(v >> 6) & 63] : '=';
        out[o++] = '=';
    }
    JSValue s = JS_NewStringLen(ctx, out, o);
    free(out);
    return s;
}

static int b64val(uint8_t c)
{
    if (c >= 'A' && c <= 'Z')
        return c - 'A';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 26;
    if (c >= '0' && c <= '9')
        return c - '0' + 52;
    if (c == '+' || c == '-')
        return 62;
    if (c == '/' || c == '_')
        return 63;
    return -1;
}

/* native.b64decode(string) -> Uint8Array (standard or URL-safe, padding optional) */
static JSValue n_b64decode(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    size_t len = 0;
    const char *s = JS_ToCStringLen(ctx, &len, argv[0]);
    if (!s)
        return JS_EXCEPTION;
    uint8_t *out = malloc(len / 4 * 3 + 3);
    if (!out) {
        JS_FreeCString(ctx, s);
        return JS_ThrowOutOfMemory(ctx);
    }
    uint32_t acc = 0;
    int bits = 0;
    size_t o = 0;
    for (size_t i = 0; i < len; i++) {
        uint8_t c = (uint8_t)s[i];
        if (c == '=' || c == ' ' || c == '\n' || c == '\r' || c == '\t')
            continue;
        int v = b64val(c);
        if (v < 0) {
            free(out);
            JS_FreeCString(ctx, s);
            return JS_ThrowTypeError(ctx, "invalid base64");
        }
        acc = (acc << 6) | (uint32_t)v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out[o++] = (uint8_t)(acc >> bits);
        }
    }
    JS_FreeCString(ctx, s);
    JSValue r = JS_NewUint8ArrayCopy(ctx, out, o);
    free(out);
    return r;
}

/* native.random(n) -> Uint8Array of n cryptographically random bytes */
static JSValue n_random(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    uint32_t n = 0;
    if (JS_ToUint32(ctx, &n, argv[0]))
        return JS_EXCEPTION;
    if (n > 65536)
        return JS_ThrowRangeError(ctx, "at most 65536 random bytes per call");
    uint8_t *buf = malloc(n ? n : 1);
    if (!buf)
        return JS_ThrowOutOfMemory(ctx);
    for (uint32_t off = 0; off < n; off += 256) {
        uint32_t chunk = n - off < 256 ? n - off : 256;
        if (getentropy(buf + off, chunk) != 0) {
            free(buf);
            return JS_ThrowInternalError(ctx, "no random source");
        }
    }
    JSValue r = JS_NewUint8ArrayCopy(ctx, buf, n);
    free(buf);
    return r;
}

static const char FORGEBASE_MODULE[] =
    "const fb = globalThis[Symbol.for('forgebase.module')];\n"
    "export const serve = fb.serve;\n"
    "export const ForgeBaseError = fb.ForgeBaseError;\n"
    "export default fb;\n";

static JSModuleDef *load_module(JSContext *ctx, const char *name, void *opaque)
{
    if (strcmp(name, "forgebase") != 0) {
        JS_ThrowReferenceError(ctx,
                               "cannot import \"%s\": functions are a single file and may import only \"forgebase\"",
                               name);
        return NULL;
    }
    JSValue m = JS_Eval(ctx, FORGEBASE_MODULE, sizeof(FORGEBASE_MODULE) - 1, "forgebase",
                        JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY);
    if (JS_IsException(m))
        return NULL;
    JSModuleDef *def = JS_VALUE_GET_PTR(m);
    JS_FreeValue(ctx, m);
    return def;
}

static char *normalize_module(JSContext *ctx, const char *base, const char *name, void *opaque)
{
    /* No path resolution: names are used as written. */
    return js_strdup(ctx, name);
}

/* Formats the pending exception as "Name: message\n<stack>" into a malloc'ed buffer. */
static char *exception_text(JSContext *ctx, size_t *out_len)
{
    JSValue exc = JS_GetException(ctx);
    JSValue str = JS_UNDEFINED;
    if (JS_IsError(exc)) {
        JSValue stack = JS_GetPropertyStr(ctx, exc, "stack");
        JSValue head = JS_ToString(ctx, exc);
        const char *h = JS_ToCString(ctx, head);
        const char *st = JS_IsString(stack) ? JS_ToCString(ctx, stack) : NULL;
        size_t hl = h ? strlen(h) : 0, sl = st ? strlen(st) : 0;
        char *buf = malloc(hl + sl + 2);
        if (buf) {
            memcpy(buf, h ? h : "", hl);
            size_t n = hl;
            if (sl) {
                buf[n++] = '\n';
                memcpy(buf + n, st, sl);
                n += sl;
            }
            buf[n] = 0;
            *out_len = n;
        }
        if (h)
            JS_FreeCString(ctx, h);
        if (st)
            JS_FreeCString(ctx, st);
        JS_FreeValue(ctx, head);
        JS_FreeValue(ctx, stack);
        JS_FreeValue(ctx, exc);
        return buf;
    }
    str = JS_ToString(ctx, exc);
    JS_FreeValue(ctx, exc);
    const char *s = JS_ToCString(ctx, str);
    JS_FreeValue(ctx, str);
    if (!s) {
        JS_FreeValue(ctx, JS_GetException(ctx));
        s = NULL;
    }
    const char *src = s ? s : "an exception that cannot be printed";
    static const char prefix[] = "Uncaught ";
    size_t pl = sizeof(prefix) - 1, n = strlen(src);
    char *buf = malloc(pl + n + 1);
    if (buf) {
        memcpy(buf, prefix, pl);
        memcpy(buf + pl, src, n);
        buf[pl + n] = 0;
        *out_len = pl + n;
    }
    if (s)
        JS_FreeCString(ctx, s);
    return buf;
}

static int32_t fail_with_exception(JSContext *ctx)
{
    size_t n = 0;
    char *msg = exception_text(ctx, &n);
    if (msg) {
        fb_output((const uint8_t *)msg, (int32_t)n);
        free(msg);
    }
    return 1;
}

static int32_t fail_with_value(JSContext *ctx, JSValue reason)
{
    JS_Throw(ctx, reason);
    return fail_with_exception(ctx);
}

/* Drains the job queue and the prelude's timers until `promise` settles. */
static int settle(JSContext *ctx, JSRuntime *rt, JSValueConst promise, JSValueConst tick)
{
    for (;;) {
        JSContext *jctx;
        int r;
        while ((r = JS_ExecutePendingJob(rt, &jctx)) > 0) {
        }
        if (r < 0)
            return -1;
        JSPromiseStateEnum st = JS_PromiseState(ctx, promise);
        if (st != JS_PROMISE_PENDING)
            return 0;
        /* tick() sleeps until the next timer and fires it; false: nothing is left to wait for. */
        JSValue more = JS_Call(ctx, tick, JS_UNDEFINED, 0, NULL);
        if (JS_IsException(more))
            return -1;
        int go = JS_ToBool(ctx, more);
        JS_FreeValue(ctx, more);
        if (!go && !JS_IsJobPending(rt)) {
            JS_ThrowInternalError(ctx, "the function's promise never settled (nothing left to wait for)");
            return -1;
        }
    }
}

/* native.now() -> monotonic milliseconds (fractional) */
static JSValue n_now(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return JS_NewFloat64(ctx, (double)ts.tv_sec * 1000.0 + (double)ts.tv_nsec / 1e6);
}

/* State built by init(); the host snapshots linear memory right after it. */
static JSRuntime *rt;
static JSContext *ctx;
static JSValue api_run, api_tick, api_begin;
static int ready;

/*
 * Creates the runtime and context and evaluates the prelude. The host calls
 * it once per process and snapshots the instance's memory afterwards; every
 * execution starts from a copy of that snapshot in a fresh instance (no
 * user code has run in it). Returns 0 on success.
 */
__attribute__((export_name("init"))) int32_t init(void)
{
    if (ready)
        return 0;
    rt = JS_NewRuntime();
    if (!rt)
        return 2;
    /* The linker reserves 1 MiB of stack; keep QuickJS's own check below it. */
    JS_SetMaxStackSize(rt, 768 * 1024);
    JS_SetModuleLoaderFunc(rt, normalize_module, load_module, NULL);
    ctx = JS_NewContext(rt);
    if (!ctx)
        return 2;
    JS_AddIntrinsicDOMException(ctx);

    JSValue global = JS_GetGlobalObject(ctx);
    JSValue native = JS_NewObject(ctx);
    JS_SetPropertyStr(ctx, native, "call", JS_NewCFunction(ctx, n_call, "call", 2));
    JS_SetPropertyStr(ctx, native, "log", JS_NewCFunction(ctx, n_log, "log", 2));
    JS_SetPropertyStr(ctx, native, "utf8encode", JS_NewCFunction(ctx, n_utf8encode, "utf8encode", 1));
    JS_SetPropertyStr(ctx, native, "utf8decode", JS_NewCFunction(ctx, n_utf8decode, "utf8decode", 3));
    JS_SetPropertyStr(ctx, native, "b64encode", JS_NewCFunction(ctx, n_b64encode, "b64encode", 1));
    JS_SetPropertyStr(ctx, native, "b64decode", JS_NewCFunction(ctx, n_b64decode, "b64decode", 1));
    JS_SetPropertyStr(ctx, native, "random", JS_NewCFunction(ctx, n_random, "random", 1));
    JS_SetPropertyStr(ctx, native, "now", JS_NewCFunction(ctx, n_now, "now", 0));
    JS_SetPropertyStr(ctx, global, "__forgebase_native", native);
    JS_FreeValue(ctx, global);

    /* The prelude returns the internal API {begin, run, tick}. */
    JSValue api = JS_Eval(ctx, PRELUDE, sizeof(PRELUDE) - 1, "forgebase:prelude", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(api))
        return fail_with_exception(ctx);
    api_begin = JS_GetPropertyStr(ctx, api, "begin");
    api_run = JS_GetPropertyStr(ctx, api, "run");
    api_tick = JS_GetPropertyStr(ctx, api, "tick");
    JS_FreeValue(ctx, api);
    JS_RunGC(rt);
    ready = 1;
    return 0;
}

__attribute__((export_name("handle"))) int32_t handle(void)
{
    if (!ready) {
        int32_t r = init();
        if (r)
            return r;
    }
    JS_UpdateStackTop(rt);
    /* Per-execution state: Math.random seed, performance.now() origin. */
    JSValue b = JS_Call(ctx, api_begin, JS_UNDEFINED, 0, NULL);
    if (JS_IsException(b))
        return fail_with_exception(ctx);
    JS_FreeValue(ctx, b);

    size_t src_len = 0;
    uint8_t *src = read_all(fb_source_len, fb_source_read, &src_len);
    if (!src) {
        static const char m[] = "cannot read the function's source";
        fb_output((const uint8_t *)m, sizeof(m) - 1);
        return 2;
    }
    JSValue mod = JS_Eval(ctx, (const char *)src, src_len, "index.js", JS_EVAL_TYPE_MODULE);
    free(src);
    if (JS_IsException(mod))
        return fail_with_exception(ctx);
    /* Module evaluation returns a promise (top-level await). */
    if (settle(ctx, rt, mod, api_tick) < 0)
        return fail_with_exception(ctx);
    if (JS_PromiseState(ctx, mod) == JS_PROMISE_REJECTED)
        return fail_with_value(ctx, JS_PromiseResult(ctx, mod));
    JS_FreeValue(ctx, mod);

    size_t in_len = 0;
    uint8_t *in = read_all(fb_input_len, fb_input_read, &in_len);
    if (!in) {
        static const char m[] = "cannot read the execution input";
        fb_output((const uint8_t *)m, sizeof(m) - 1);
        return 2;
    }
    JSValue input = JS_NewStringLen(ctx, (const char *)in, in_len);
    free(in);
    JSValue p = JS_Call(ctx, api_run, JS_UNDEFINED, 1, (JSValueConst *)&input);
    JS_FreeValue(ctx, input);
    if (JS_IsException(p))
        return fail_with_exception(ctx);
    if (settle(ctx, rt, p, api_tick) < 0)
        return fail_with_exception(ctx);
    JSValue result = JS_PromiseResult(ctx, p);
    if (JS_PromiseState(ctx, p) == JS_PROMISE_REJECTED)
        return fail_with_value(ctx, result);
    size_t out_len = 0;
    const char *out = JS_ToCStringLen(ctx, &out_len, result);
    if (!out)
        return fail_with_exception(ctx);
    int32_t ok = fb_output((const uint8_t *)out, (int32_t)out_len);
    /* No teardown: the instance is discarded after this call. */
    return ok == 0 ? 0 : 3;
}
