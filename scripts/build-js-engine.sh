#!/usr/bin/env bash
# Builds the embedded JavaScript engine of ForgeBase functions (ADR-0027):
# QuickJS (quickjs-ng) plus crates/forgebase-functions/js/engine.c and
# prelude.js, compiled for wasm32-wasip1 with a pinned wasi-sdk.
#
#   scripts/build-js-engine.sh           rebuild and update the committed artifact
#   scripts/build-js-engine.sh --check   rebuild into a temporary file and fail if
#                                        its SHA-256 differs from the committed one
#
# Tools and sources are downloaded once into $JS_ENGINE_TOOLS (default
# .cache/js-engine-tools in the repository) and verified by SHA-256. Users of
# ForgeBase never need any of this: the artifact is committed and embedded
# with include_bytes!.
set -euo pipefail

WASI_SDK_VERSION=34.0
WASI_SDK_TAG=wasi-sdk-34
QUICKJS_NG_VERSION=0.17.0
QUICKJS_NG_SHA256=559bc4c420475e55c7ab4510adbc562f55d7524d75e8e89d79ce4bb02f5687d9

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JS_DIR="$ROOT/crates/forgebase-functions/js"
ARTIFACT="$JS_DIR/forgebase-js.wasm"
CHECKSUM="$JS_DIR/forgebase-js.wasm.sha256"
TOOLS="${JS_ENGINE_TOOLS:-$ROOT/.cache/js-engine-tools}"

check=0
case "${1:-}" in
  --check) check=1 ;;
  "") ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) SDK_ASSET="wasi-sdk-$WASI_SDK_VERSION-arm64-macos"; SDK_SHA256=9c59398106b417f8f14913380fdf0097a8cc0ff4af9eb3ce0065a859e88d49e9 ;;
  Darwin-x86_64) SDK_ASSET="wasi-sdk-$WASI_SDK_VERSION-x86_64-macos"; SDK_SHA256=87d27fa8adc68dee59bfbf2e22a6d34ef717c34d6bf1d8af2a56fc929d9ce0eb ;;
  Linux-x86_64) SDK_ASSET="wasi-sdk-$WASI_SDK_VERSION-x86_64-linux"; SDK_SHA256=b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4 ;;
  Linux-aarch64) SDK_ASSET="wasi-sdk-$WASI_SDK_VERSION-arm64-linux"; SDK_SHA256=f7e243dff54d60bcc576e94d6166b69f410f2500ae4a9ceef34315be10e77971 ;;
  *) echo "unsupported build host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

fetch() { # url file sha256
  local url="$1" file="$2" want="$3"
  if [ ! -f "$file" ] || [ "$(sha256 "$file")" != "$want" ]; then
    echo "downloading $url" >&2
    curl -fsSL --retry 3 -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  local got
  got="$(sha256 "$file")"
  if [ "$got" != "$want" ]; then
    echo "checksum mismatch for $file: expected $want, got $got" >&2
    exit 1
  fi
}

mkdir -p "$TOOLS"
SDK_TGZ="$TOOLS/$SDK_ASSET.tar.gz"
QJS_TGZ="$TOOLS/quickjs-ng-$QUICKJS_NG_VERSION.tar.gz"
fetch "https://github.com/WebAssembly/wasi-sdk/releases/download/$WASI_SDK_TAG/$SDK_ASSET.tar.gz" "$SDK_TGZ" "$SDK_SHA256"
fetch "https://github.com/quickjs-ng/quickjs/archive/refs/tags/v$QUICKJS_NG_VERSION.tar.gz" "$QJS_TGZ" "$QUICKJS_NG_SHA256"
SDK="$TOOLS/$SDK_ASSET"
QJS="$TOOLS/quickjs-$QUICKJS_NG_VERSION"
[ -x "$SDK/bin/clang" ] || tar -xzf "$SDK_TGZ" -C "$TOOLS"
[ -f "$QJS/quickjs.c" ] || tar -xzf "$QJS_TGZ" -C "$TOOLS"

OUT="$ARTIFACT"
if [ "$check" = 1 ]; then
  OUT="$(mktemp "${TMPDIR:-/tmp}/forgebase-js.XXXXXX")"
  trap 'rm -f "$OUT"' EXIT
fi

# Reproducibility: fixed tool versions, no debug info, paths mapped to stable
# names, no timestamps (wasm-ld writes none), sources in a fixed order.
CFLAGS=(
  --target=wasm32-wasip1
  "--sysroot=$SDK/share/wasi-sysroot"
  -std=gnu23 -O2 -g0 -DNDEBUG -D_GNU_SOURCE -D_WASI_EMULATED_PROCESS_CLOCKS
  -fno-ident
  "-ffile-prefix-map=$QJS=quickjs"
  "-ffile-prefix-map=$JS_DIR=js"
  -Wno-unknown-attributes
  -I "$QJS"
)
LDFLAGS=(
  -mexec-model=reactor
  -Wl,-z,stack-size=1048576
  -Wl,--export=__stack_pointer
  -Wl,--strip-all
  -Wl,--gc-sections
)
"$SDK/bin/clang" "${CFLAGS[@]}" "${LDFLAGS[@]}" \
  "$QJS/dtoa.c" "$QJS/libregexp.c" "$QJS/libunicode.c" "$QJS/quickjs.c" \
  "$JS_DIR/engine.c" \
  --embed-dir="$JS_DIR" \
  -o "$OUT" -lm

got="$(sha256 "$OUT")"
if [ "$check" = 1 ]; then
  want="$(cut -d' ' -f1 "$CHECKSUM")"
  if [ "$got" != "$want" ]; then
    echo "the JavaScript engine does not rebuild to the committed artifact:" >&2
    echo "  committed $want" >&2
    echo "  rebuilt   $got" >&2
    exit 1
  fi
  echo "js engine reproducible: $got"
else
  echo "$got  forgebase-js.wasm" > "$CHECKSUM"
  echo "built $ARTIFACT ($(wc -c < "$ARTIFACT" | tr -d ' ') bytes, sha256 $got)"
fi
