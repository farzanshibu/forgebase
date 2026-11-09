#!/usr/bin/env python3
"""Generates src/generated.rs (wire models) from the server's OpenAPI document.

    python3 scripts/generate_models.py [../js/openapi/openapi.json] [src/generated.rs]

The output is committed; CI-style drift check: run it and `git diff --exit-code src/generated.rs`.
Hand-written code (client, auth, records, realtime, ...) lives in the other modules and uses these
models for typed responses. Rules, kept deliberately simple:

* object schema           -> struct (optional or nullable fields -> Option<T>, unknown fields ignored)
* string enum             -> enum with a catch-all `Unknown` variant (forward compatible)
* oneOf of $refs          -> untagged enum; any other oneOf/anyOf -> serde_json::Value
* allOf                   -> one struct with the merged properties
* free-form object / {}   -> serde_json::Value
"""
import json
import re
import sys

SRC = sys.argv[1] if len(sys.argv) > 1 else "../js/openapi/openapi.json"
OUT = sys.argv[2] if len(sys.argv) > 2 else "src/generated.rs"

spec = json.load(open(SRC))
schemas = spec["components"]["schemas"]

RESERVED_TYPES = {"Option", "Result", "Vec", "String", "Box", "Value", "Self", "HashMap", "Some", "None", "Ok", "Err"}
KEYWORDS = {
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super",
    "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become",
    "box", "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
}


def pascal(s):
    parts = re.split(r"[^0-9A-Za-z]+", s)
    out = "".join(p[:1].upper() + p[1:] for p in parts if p)
    if not out:
        out = "Empty"
    if out[0].isdigit():
        out = "V" + out
    return out


def snake(s):
    s = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", s)
    s = re.sub(r"[^0-9A-Za-z]+", "_", s).strip("_").lower()
    if not s:
        s = "field"
    if s[0].isdigit():
        s = "f_" + s
    return s


def tname(n):
    p = pascal(n)
    return p + "Model" if p in RESERVED_TYPES else p


def doc(s, indent=""):
    if not s:
        return ""
    lines = [l.rstrip() for l in s.strip().splitlines()]
    return "".join(f"{indent}/// {l}\n" if l else f"{indent}///\n" for l in lines)


def ref_name(s):
    return s["$ref"].rsplit("/", 1)[-1]


def is_null(s):
    return isinstance(s, dict) and (s.get("type") == "null")


def resolve(s):
    while isinstance(s, dict) and "$ref" in s:
        s = schemas[ref_name(s)]
    return s


# ---- recursion detection (box direct self references) ---------------------------------------


def refs_of(s, acc):
    if isinstance(s, dict):
        if "$ref" in s:
            acc.add(ref_name(s))
        for v in s.values():
            refs_of(v, acc)
    elif isinstance(s, list):
        for v in s:
            refs_of(v, acc)


graph = {}
for n, s in schemas.items():
    a = set()
    refs_of(s, a)
    graph[n] = a


def reaches(a, b, seen=None):
    seen = seen or set()
    if a == b and seen:
        return True
    for n in graph.get(a, ()):
        if n in seen:
            continue
        seen.add(n)
        if n == b or reaches(n, b, seen):
            return True
    return False


# ---- type mapping -----------------------------------------------------------------------------

extra_defs = []  # generated inline enums
default_ok = {}  # schema name -> bool (computed after)
struct_fields = {}  # name -> list of (field, type string)


def prim(s):
    t = s.get("type")
    if isinstance(t, list):
        t = [x for x in t if x != "null"]
        t = t[0] if len(t) == 1 else None
    if t == "string":
        return "String"
    if t == "integer":
        return "i64"
    if t == "number":
        return "f64"
    if t == "boolean":
        return "bool"
    return None


def nullable(s):
    t = s.get("type")
    if isinstance(t, list) and "null" in t:
        return True
    for k in ("oneOf", "anyOf"):
        if k in s and any(is_null(x) for x in s[k]):
            return True
    return False


def map_type(s, owner, field, boxed_owner=None):
    """Returns (rust type, is_option)."""
    if not isinstance(s, dict) or not s:
        return "serde_json::Value", False
    opt = nullable(s)
    if "$ref" in s:
        n = ref_name(s)
        ty = tname(n)
        if boxed_owner and (n == boxed_owner or reaches(n, boxed_owner)):
            ty = f"Box<{ty}>"
        return ty, opt
    for k in ("oneOf", "anyOf"):
        if k in s:
            non_null = [x for x in s[k] if not is_null(x)]
            if len(non_null) == 1:
                ty, o = map_type(non_null[0], owner, field, boxed_owner)
                return ty, opt or o
            return "serde_json::Value", opt
    if "allOf" in s:
        if len(s["allOf"]) == 1:
            return map_type(s["allOf"][0], owner, field, boxed_owner)
        return "serde_json::Value", opt
    if "enum" in s and prim(s) == "String" and all(isinstance(x, str) for x in s["enum"] if x is not None):
        name = tname(owner + "_" + field)
        gen_enum(name, [x for x in s["enum"] if x is not None], s.get("description"))
        return name, opt or None in s["enum"]
    t = s.get("type")
    if isinstance(t, list):
        t = [x for x in t if x != "null"]
        t = t[0] if len(t) == 1 else None
    if t == "array":
        inner, _ = map_type(s.get("items", {}), owner, field + "_item")
        return f"Vec<{inner}>", opt
    if t == "object" or "properties" in s or "additionalProperties" in s:
        if "properties" in s:
            name = tname(owner + "_" + field)
            gen_struct(name, s)
            return name, opt
        ap = s.get("additionalProperties")
        if isinstance(ap, dict) and ap:
            inner, _ = map_type(ap, owner, field + "_value")
            return f"std::collections::HashMap<String, {inner}>", opt
        return "serde_json::Value", opt
    p = prim(s)
    if p:
        return p, opt
    return "serde_json::Value", opt


# ---- emission ---------------------------------------------------------------------------------

out_defs = {}  # name -> source


def variant(v):
    p = pascal(v)
    if p in ("Self", "Unknown"):
        p += "Value"
    return p


def gen_enum(name, values, description=None):
    if name in out_defs:
        return
    seen = set()
    lines = [doc(description), "#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\n", f"pub enum {name} {{\n"]
    for v in values:
        vn = variant(v)
        while vn in seen:
            vn += "_"
        seen.add(vn)
        lines.append(f'    #[serde(rename = "{v}")]\n    {vn},\n')
    lines.append("    /// A value this SDK version does not know (kept so newer servers do not break old clients).\n")
    lines.append("    #[serde(other)]\n    Unknown,\n}\n")
    out_defs[name] = "".join(lines)


def gen_struct(name, s):
    if name in out_defs:
        return
    out_defs[name] = ""  # reserve (recursion)
    props = {}
    required = set()

    def collect(x):
        x = x if "$ref" not in x else schemas[ref_name(x)]
        if "allOf" in x:
            for y in x["allOf"]:
                collect(y)
        required.update(x.get("required", []))
        for k, v in x.get("properties", {}).items():
            props[k] = v

    collect(s)
    lines = [doc(s.get("description"))]
    fields = []
    body = []
    used = set()
    for k, v in props.items():
        ty, opt = map_type(v, name, k, boxed_owner=None)
        # box a direct reference back to this struct
        if ty.startswith("Vec<") or ty.startswith("std::collections"):
            pass
        else:
            base = ty
            if "$ref" in v:
                rn = ref_name(v)
                if rn in schemas and (tname(rn) == name or reaches(rn, name)) and name in schemas:
                    base = f"Box<{tname(rn)}>"
                    ty = base
        is_opt = opt or k not in required
        fname = snake(k)
        if fname in KEYWORDS:
            fname = "r#" + fname if fname not in ("self", "crate", "super") else fname + "_"
        while fname in used:
            fname += "_"
        used.add(fname)
        rust_ty = f"Option<{ty}>" if is_opt else ty
        attrs = ""
        raw = fname[2:] if fname.startswith("r#") else fname
        if raw != k:
            attrs += f'    #[serde(rename = "{k}")]\n'
        if is_opt:
            attrs += '    #[serde(default, skip_serializing_if = "Option::is_none")]\n'
        elif ty.startswith("Vec<") or ty.startswith("std::collections") or ty == "serde_json::Value":
            attrs += "    #[serde(default)]\n"
        body.append(doc(v.get("description") if isinstance(v, dict) else None, "    ") + attrs + f"    pub {fname}: {rust_ty},\n")
        fields.append((fname, rust_ty))
    struct_fields[name] = fields
    lines.append("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n")
    lines.append(f"pub struct {name} {{\n" + "".join(body) + "}\n")
    out_defs[name] = "".join(lines)


def gen_top(n, s):
    name = tname(n)
    if "oneOf" in s or "anyOf" in s:
        k = "oneOf" if "oneOf" in s else "anyOf"
        alts = s[k]
        if all("$ref" in a for a in alts):
            lines = [doc(s.get("description")), "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n", "#[serde(untagged)]\n", f"pub enum {name} {{\n"]
            for a in alts:
                rn = ref_name(a)
                inner = tname(rn)
                if reaches(rn, n):
                    inner = f"Box<{inner}>"
                lines.append(f"    /// `{rn}`\n    {tname(rn)}({inner}),\n")
            lines.append("}\n")
            out_defs[name] = "".join(lines)
        else:
            out_defs[name] = doc(s.get("description")) + f"pub type {name} = serde_json::Value;\n"
        return
    if "enum" in s and prim(s) == "String":
        gen_enum(name, [x for x in s["enum"] if x is not None], s.get("description"))
        return
    if "properties" in s or "allOf" in s:
        gen_struct(name, s)
        return
    ty, opt = map_type(s, n, "value")
    out_defs[name] = doc(s.get("description")) + f"pub type {name} = {ty};\n"


for n, s in schemas.items():
    gen_top(n, s)

# Default derive where every field type supports it.
DEFAULT_PRIMS = ("String", "i64", "f64", "bool", "serde_json::Value")


def can_default(name, stack=()):
    if name in default_ok:
        return default_ok[name]
    if name in stack:
        return False
    ok = True
    for _, ty in struct_fields.get(name, []):
        t = ty
        if t.startswith("Option<") or t.startswith("Vec<") or t.startswith("std::collections"):
            continue
        if t in DEFAULT_PRIMS:
            continue
        if t in struct_fields and can_default(t, stack + (name,)):
            continue
        ok = False
        break
    default_ok[name] = ok
    return ok


final = []
for name in sorted(out_defs):
    src = out_defs[name]
    if name in struct_fields and can_default(name):
        src = src.replace("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\npub struct", "#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]\npub struct", 1)
    final.append(src)

header = """//! Wire models generated from the server's OpenAPI document by `scripts/generate_models.py`.
//!
//! Do not edit by hand: change the script or the OpenAPI source and regenerate.

#![allow(missing_docs, clippy::large_enum_variant, clippy::enum_variant_names, clippy::struct_field_names)]

use serde::{Deserialize, Serialize};

"""
open(OUT, "w").write(header + "\n".join(final))
print(f"{len(out_defs)} models -> {OUT}")
