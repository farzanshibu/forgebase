#!/usr/bin/env bash
# Builds the bundled PostgreSQL engine archive for one macOS or Linux (musl)
# target: pinned sources -> static compression libraries, shared OpenSSL ->
# PostgreSQL + the contrib list + pgvector, pg_cron, pgmq (+ any extension in
# extensions/) -> pruned, relocatable tree -> forgebase-pg<ver>-<build>-<target>.tar.zst.
#
#   deploy/postgres-engine/build-unix.sh <target> [work dir] [out dir]
#
# Targets: aarch64-apple-darwin, x86_64-apple-darwin (on macOS),
#          x86_64-unknown-linux-musl, aarch64-unknown-linux-musl (inside the
#          Alpine build image: deploy/postgres-engine/build-linux.sh runs it).
# Env: JOBS (parallel make), FB_ENGINE_SOURCES (download cache, default
#      <work>/src), FB_ENGINE_STEPS (stop after this step, for debugging).
# Runs on bash 3.2 (macOS) and later. Re-running resumes: each finished step leaves a stamp in <work>/stamps.
set -euo pipefail
export LC_ALL=C TZ=UTC

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd); export FORGEBASE_PG_ENGINE_LOCK=${FORGEBASE_PG_ENGINE_LOCK:-$ROOT/engine.lock}
TARGET=${1:?usage: build-unix.sh <target> [work dir] [out dir]}
WORK_REAL=$(mkdir -p "${2:-$PWD/engine-work/$TARGET}" && cd "${2:-$PWD/engine-work/$TARGET}" && pwd)
OUT=$(mkdir -p "${3:-$WORK_REAL/out}" && cd "${3:-$WORK_REAL/out}" && pwd)
SRC=${FB_ENGINE_SOURCES:-$WORK_REAL/src}
# The build sees its work directory through a fixed path, so the paths
# PostgreSQL records (pg_config's configure arguments and flags, compiled
# into postgres) are the same wherever the work directory is: the archive
# bytes depend on the sources and the toolchain only. One build per target
# at a time on a machine.
WORK=${FB_ENGINE_LINK:-/tmp/forgebase-engine-$TARGET}
if [ "$WORK" != "$WORK_REAL" ]; then
  rm -f "$WORK"
  ln -s "$WORK_REAL" "$WORK"
fi
cd "$WORK"
JOBS=${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}
PY=${PYTHON:-python3}

lock() { "$PY" "$HERE/engine.py" get "$1"; }
log() { printf '\n=== %s [%s]\n' "$*" "$(date -u +%H:%M:%S)"; }
stamp() { [ -f "$WORK/stamps/$1" ]; }
done_step() { mkdir -p "$WORK/stamps"; touch "$WORK/stamps/$1"; [ "${FB_ENGINE_STEPS:-}" != "$1" ] || { echo "stopped after $1"; exit 0; }; }

PGVER=$(lock engine.postgres)
BUILD=$(lock engine.build)
PROGRAMS=$(lock contents.programs)
CONTRIB=$(lock contents.contrib)
EXTENSIONS=$(lock contents.extensions)
# A fixed epoch keeps OpenSSL's "built on" string (and anything else that
# reads it) the same on every build.
export SOURCE_DATE_EPOCH=1762819200

case "$TARGET" in
  aarch64-apple-darwin) OS=macos ARCH=arm64 SSL_TARGET=darwin64-arm64-cc ;;
  x86_64-apple-darwin) OS=macos ARCH=x86_64 SSL_TARGET=darwin64-x86_64-cc ;;
  x86_64-unknown-linux-musl) OS=linux ARCH=x86_64 SSL_TARGET=linux-x86_64 ;;
  aarch64-unknown-linux-musl) OS=linux ARCH=aarch64 SSL_TARGET=linux-aarch64 ;;
  *) echo "unknown target $TARGET" >&2; exit 2 ;;
esac

# The install prefix contains "postgres", so PostgreSQL puts modules in lib/
# and data in share/ (no postgresql/ subdirectories), the layout
# forgebase-pgengine expects. It is only a name: everything is installed
# under DESTDIR and PostgreSQL finds share/ and lib/ relative to its
# executable after extraction.
PREFIX=/forgebase-postgres
DEPS=$WORK/deps
STAGE=$WORK/stage
ENGINE=$WORK/engine
B=$WORK/build
STAGED=$STAGE$PREFIX
# 255-byte placeholder interpreter, rewritten in place at extraction (ADR-0028 §2.3).
PLACEHOLDER=/forgebase-pg-loader-$(printf 'x%.0s' $(seq 1 234))
[ ${#PLACEHOLDER} -eq 255 ] || { echo "placeholder is ${#PLACEHOLDER} bytes" >&2; exit 1; }

MAKEVARS=()
HOSTFLAG=()
CFLAGS_COMMON="-O2 -ffile-prefix-map=$WORK=/fb"
if [ "$OS" = macos ]; then
  MACOSX_DEPLOYMENT_TARGET=$(lock engine.macos_deployment_target); export MACOSX_DEPLOYMENT_TARGET
  HOST_ARCH=$(uname -m)
  export CC="clang -arch $ARCH"
  if [ "$HOST_ARCH" != "$ARCH" ]; then
    # Cross build (x86_64 on Apple silicon): configure runs no test programs;
    # the smoke test then needs Rosetta 2.
    HOSTFLAG=(--host="$TARGET")
  fi
  LDFLAGS_COMMON="-Wl,-headerpad_max_install_names -Wl,-dead_strip_dylibs"
  DLL=dylib
else
  [ -f /etc/alpine-release ] || { echo "Linux engines are built in the Alpine image (build-linux.sh)" >&2; exit 2; }
  [ "$(uname -m)" = "$ARCH" ] || { echo "host is $(uname -m), target needs $ARCH" >&2; exit 2; }
  export CC=gcc
  CFLAGS_COMMON="$CFLAGS_COMMON -fPIC"
  LDFLAGS_COMMON=""
  # RUNPATH $ORIGIN/../lib works from bin/ and from lib/ (lib/../lib).
  MAKEVARS=("rpathdir=\$\$ORIGIN/../lib")
  DLL=so
fi

mkdir -p "$SRC" "$B" "$DEPS/bin" "$OUT"
echo "target $TARGET ($OS/$ARCH) work $WORK -> $WORK_REAL jobs $JOBS"

unpack() { # <source name> -> prints the unpacked directory
  local name=$1 ver file dir
  ver=$(lock "sources.$name.version")
  file=$(ls "$SRC/$name-$ver".tar.* 2>/dev/null | head -1)
  dir=$(lock "sources.$name.dir" 2>/dev/null || echo "$name-$ver")
  [ "$name" = postgresql ] && dir="postgresql-$ver"
  if [ ! -d "$B/$dir" ]; then
    tar -C "$B" -xf "$file"
  fi
  echo "$B/$dir"
}

# ---------------------------------------------------------------- sources
if ! stamp fetch; then
  log "fetch and verify sources"
  "$PY" "$HERE/engine.py" fetch --dest "$SRC" --os "$OS"
  done_step fetch
fi

# ---------------------------------------------------------------- dependencies
if ! stamp openssl; then
  log "OpenSSL (shared)"
  d=$(unpack openssl)
  ( cd "$d"
    extra=()
    [ "$OS" = linux ] && extra=(-fPIC)
    # A fixed prefix keeps build paths out of libcrypto (ENGINESDIR,
    # MODULESDIR); the files are installed through DESTDIR into deps/.
    CC=${CC%% *} ./Configure "$SSL_TARGET" shared no-module no-tests no-docs \
      --prefix=/forgebase-openssl --libdir=lib --openssldir=/etc/ssl "${extra[@]+"${extra[@]}"}" $LDFLAGS_COMMON
    make -j"$JOBS" build_libs >/dev/null
    rm -rf "$WORK/openssl-dest"
    make install_dev DESTDIR="$WORK/openssl-dest" >/dev/null
    cp -RP "$WORK/openssl-dest/forgebase-openssl/." "$DEPS/" )
  done_step openssl
fi

# The build runs programs linked with the shared OpenSSL (configure's test
# programs, zic, pg_config). macOS: give the libraries their real deps/ path
# as install name for now (rewritten to @loader_path when the engine is
# assembled); DYLD_LIBRARY_PATH would not survive SIP. Linux: LD_LIBRARY_PATH.
if [ "$OS" = macos ]; then
  for l in libssl.3 libcrypto.3; do
    install_name_tool -id "$DEPS/lib/$l.dylib" "$DEPS/lib/$l.dylib" 2>/dev/null
  done
  install_name_tool -change /forgebase-openssl/lib/libcrypto.3.dylib "$DEPS/lib/libcrypto.3.dylib" \
    "$DEPS/lib/libssl.3.dylib" 2>/dev/null
  codesign --force --sign - "$DEPS/lib/libssl.3.dylib" "$DEPS/lib/libcrypto.3.dylib" 2>/dev/null
else
  export LD_LIBRARY_PATH=$DEPS/lib
fi

if [ "$OS" = linux ] && ! stamp zlib; then
  log "zlib (static)"
  d=$(unpack zlib)
  ( cd "$d" && CFLAGS="$CFLAGS_COMMON" ./configure --static --prefix="$DEPS" >/dev/null && make -j"$JOBS" >/dev/null && make install >/dev/null )
  done_step zlib
fi

if ! stamp lz4; then
  log "lz4 (static)"
  d=$(unpack lz4)
  make -C "$d/lib" -j"$JOBS" BUILD_SHARED=no CFLAGS="$CFLAGS_COMMON" PREFIX="$DEPS" install >/dev/null
  done_step lz4
fi

if ! stamp zstd; then
  log "zstd (static library; CLI for packing)"
  d=$(unpack zstd)
  make -C "$d/lib" -j"$JOBS" CFLAGS="$CFLAGS_COMMON" libzstd.a-mt >/dev/null
  make -C "$d/lib" PREFIX="$DEPS" install-static install-includes >/dev/null
  # The packer's zstd runs on the build host: plain host compiler.
  make -C "$d/programs" -j"$JOBS" CC=cc HAVE_ZLIB=0 HAVE_LZMA=0 HAVE_LZ4=0 zstd >/dev/null
  cp "$d/programs/zstd" "$DEPS/bin/zstd"
  done_step zstd
fi

if [ "$OS" = linux ] && ! stamp libedit; then
  log "libedit (static, ncursesw static)"
  d=$(unpack libedit)
  ( cd "$d" && CFLAGS="$CFLAGS_COMMON" ./configure --prefix="$DEPS" --disable-shared --enable-static \
      --disable-examples >/dev/null && make -j"$JOBS" >/dev/null && make install >/dev/null )
  done_step libedit
fi

# ---------------------------------------------------------------- PostgreSQL
PG=$B/postgresql-$PGVER
if ! stamp postgres; then
  log "PostgreSQL $PGVER configure"
  d=$(unpack postgresql)
  LIBS_EXTRA=""
  [ "$OS" = linux ] && LIBS_EXTRA="-Wl,-Bstatic -lncursesw -Wl,-Bdynamic"
  ( cd "$d"
    ./configure --prefix="$PREFIX" "${HOSTFLAG[@]+"${HOSTFLAG[@]}"}" \
      --with-ssl=openssl --with-lz4 --with-zstd --with-zlib --with-readline --with-libedit-preferred \
      --without-icu --without-llvm --without-perl --without-python --without-tcl \
      --without-gssapi --without-ldap --without-pam --without-bonjour --without-selinux \
      --without-systemd --without-libxml --without-libxslt --without-libcurl \
      --without-liburing --without-libnuma --disable-nls \
      --with-includes="$DEPS/include" --with-libraries="$DEPS/lib" \
      CFLAGS="$CFLAGS_COMMON" LDFLAGS="-L$DEPS/lib $LDFLAGS_COMMON" LIBS="$LIBS_EXTRA" \
      LZ4_CFLAGS="-I$DEPS/include" LZ4_LIBS="$DEPS/lib/liblz4.a" \
      ZSTD_CFLAGS="-I$DEPS/include" ZSTD_LIBS="$DEPS/lib/libzstd.a" > "$WORK/configure.log" \
      || { tail -40 config.log; exit 1; } )
  grep -E '^#define (USE_OPENSSL|USE_LZ4|USE_ZSTD|HAVE_LIBZ|USE_EDITLINE|HAVE_LIBEDIT|HAVE_EDITLINE_READLINE_H|HAVE_READLINE_H)' "$PG/src/include/pg_config.h" || true
  log "PostgreSQL build"
  make -C "$PG" -j"$JOBS" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" >"$WORK/make.log" 2>&1 || { tail -60 "$WORK/make.log"; exit 1; }
  rm -rf "$STAGE"
  make -C "$PG" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" install DESTDIR="$STAGE" >>"$WORK/make.log" 2>&1 || { tail -40 "$WORK/make.log"; exit 1; }
  done_step postgres
fi

if ! stamp contrib; then
  log "contrib: $CONTRIB"
  for c in $CONTRIB; do
    make -C "$PG/contrib/$c" -j"$JOBS" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" >>"$WORK/make.log" 2>&1 \
      && make -C "$PG/contrib/$c" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" install DESTDIR="$STAGE" >>"$WORK/make.log" 2>&1 \
      || { tail -40 "$WORK/make.log"; exit 1; }
  done
  done_step contrib
fi

# ---------------------------------------------------------------- extensions
# Each extension is a PGXS build against the staged engine (its pg_config
# finds the staged paths relative to itself). The hook for one more
# extension with patches (Apache AGE, FB-160): add [sources.<name>] to
# engine.lock, list <name> in contents.extensions, and add
# deploy/postgres-engine/<name>/ (or extensions/<name>/) with any of
#   patches/*.patch   applied in order with `patch -p1` before building
#   make-args         extra make arguments, one per line
#   post-install.sh   run after `make install` (env: STAGED, OS, TARGET, DLL, EXT_DIR)
# A directory with build.sh instead replaces the default PGXS build.
PG_CONFIG=$STAGED/bin/pg_config
# pg_config reports its real (symlink-resolved) location, so map that path
# too; COPT is added to CFLAGS by PGXS without being recorded anywhere.
EXT_COPT="-ffile-prefix-map=$WORK_REAL=/fb -ffile-prefix-map=$(cd "$WORK_REAL" && pwd -P)=/fb"
build_extension() {
  local name=$1 d hook=$HERE/extensions/$1 extra=()
  [ -d "$HERE/$name" ] && hook=$HERE/$name   # deploy/postgres-engine/<name>/ (FB-160: age/)
  d=$(unpack "$name")
  [ "$name" = pgmq ] && d=$d/pgmq-extension
  if [ -x "$hook/build.sh" ]; then
    STAGED=$STAGED OS=$OS TARGET=$TARGET DLL=$DLL EXT_DIR=$d PG_CONFIG=$PG_CONFIG JOBS=$JOBS \
      "$hook/build.sh"
    return
  fi
  if [ -d "$hook/patches" ] && [ ! -f "$d/.fb-patched" ]; then
    for p in "$hook"/patches/*.patch; do
      [ -f "$p" ] || continue
      echo "patch $(basename "$p")"
      patch -d "$d" -p1 --forward < "$p"
    done
    touch "$d/.fb-patched"
  fi
  if [ -f "$hook/make-args" ]; then
    while IFS= read -r line; do [ -n "$line" ] && extra+=("$line"); done < "$hook/make-args"
  fi
  make -C "$d" -j"$JOBS" PG_CONFIG="$PG_CONFIG" OPTFLAGS="" COPT="$EXT_COPT" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" "${extra[@]+"${extra[@]}"}" \
    >>"$WORK/make.log" 2>&1 || { tail -40 "$WORK/make.log"; exit 1; }
  make -C "$d" PG_CONFIG="$PG_CONFIG" OPTFLAGS="" COPT="$EXT_COPT" "${MAKEVARS[@]+"${MAKEVARS[@]}"}" "${extra[@]+"${extra[@]}"}" install \
    >>"$WORK/make.log" 2>&1 || { tail -40 "$WORK/make.log"; exit 1; }
  if [ -x "$hook/post-install.sh" ]; then
    STAGED=$STAGED OS=$OS TARGET=$TARGET DLL=$DLL EXT_DIR=$d "$hook/post-install.sh"
  fi
}
for ext in $EXTENSIONS; do
  if ! stamp "ext-$ext"; then
    log "extension $ext $(lock "sources.$ext.version")"
    build_extension "$ext"
    done_step "ext-$ext"
  fi
done

# ---------------------------------------------------------------- relink (Linux)
# Executables are first linked with musl's normal loader so the build can run
# them (zic for the time zone data, pg_config for the extensions); the
# shipped ones are then relinked with the padded placeholder interpreter.
if [ "$OS" = linux ] && ! stamp relink; then
  log "relink executables with the placeholder PT_INTERP"
  for p in $PROGRAMS; do
    if [ "$p" = postgres ]; then dir=$PG/src/backend; else
      dir=$(dirname "$(find "$PG/src/bin" -mindepth 2 -maxdepth 2 -type f -name "$p" | head -1)")
    fi
    [ -f "$dir/$p" ] || { echo "no built program $p" >&2; exit 1; }
    rm -f "$dir/$p"
    make -C "$dir" "${MAKEVARS[@]}" LDFLAGS_EX="-Wl,--dynamic-linker=$PLACEHOLDER" "$p" >>"$WORK/make.log" 2>&1 \
      || { tail -40 "$WORK/make.log"; exit 1; }
    install -m 755 "$dir/$p" "$STAGED/bin/$p"
  done
  done_step relink
fi

# ---------------------------------------------------------------- engine tree
log "assemble the engine tree"
rm -rf "$ENGINE"
mkdir -p "$ENGINE/bin" "$ENGINE/lib" "$ENGINE/share"
for p in $PROGRAMS; do cp -p "$STAGED/bin/$p" "$ENGINE/bin/$p"; done
# lib/: server modules and libpq; not static or development files.
( cd "$STAGED/lib"
  for f in *; do
    case "$f" in
      *.a|pkgconfig|pgxs|libecpg*|libpgtypes*|libpq.so|libpq.dylib) continue ;;
    esac
    cp -RP "$f" "$ENGINE/lib/"
  done )
# Shared OpenSSL, under the names the binaries record.
if [ "$OS" = macos ]; then
  cp -p "$DEPS/lib/libssl.3.dylib" "$DEPS/lib/libcrypto.3.dylib" "$ENGINE/lib/"
else
  cp -p "$DEPS/lib/libssl.so.3" "$DEPS/lib/libcrypto.so.3" "$ENGINE/lib/"
  cp -L "/lib/ld-musl-$ARCH.so.1" "$ENGINE/lib/ld-musl-$ARCH.so.1"
fi
cp -RP "$STAGED/share/." "$ENGINE/share/"
rm -rf "$ENGINE/share/doc" "$ENGINE/share/man"
chmod -R u+w "$ENGINE"
"$PY" "$HERE/engine.py" licences --os "$OS" --into "$ENGINE/share/licenses"

if [ "$OS" = macos ]; then
  log "macOS: strip, @loader_path install names, ad-hoc signatures"
  machos=$(cd "$ENGINE" && find bin lib -type f | while read -r f; do
    file -b "$f" | grep -q 'Mach-O' && echo "$f"; done)
  for f in $machos; do
    p=$ENGINE/$f
    case "$f" in bin/*) strip -x "$p" 2>/dev/null || strip "$p" ;; *) strip -x "$p" ;; esac
    case "$f" in lib/*.dylib) install_name_tool -id "@rpath/$(basename "$f")" "$p" 2>/dev/null || true ;; esac
    for dep in $(otool -L "$p" | tail -n +2 | awk '{print $1}'); do
      case "$dep" in
        "$PREFIX"/lib/*|/forgebase-openssl/lib/*|"$DEPS"/lib/*)
          install_name_tool -change "$dep" "@loader_path/../lib/$(basename "$dep")" "$p" 2>/dev/null ;;
      esac
    done
    codesign --force --sign - "$p" 2>/dev/null
  done
  log "macOS: check dependencies and signatures"
  bad=0
  for f in $machos; do
    for dep in $(otool -L "$ENGINE/$f" | tail -n +2 | awk '{print $1}'); do
      case "$dep" in
        /usr/lib/*|/System/*|@loader_path/../lib/*|@rpath/*) ;;
        *) echo "  $f -> $dep (outside the engine and the OS)"; bad=1 ;;
      esac
    done
    codesign --verify "$ENGINE/$f" || bad=1
  done
  [ $bad -eq 0 ] || exit 1
  otool -L "$ENGINE/bin/psql" "$ENGINE/bin/postgres"
else
  log "Linux: strip; check interpreter, RUNPATH and needed libraries"
  bad=0
  ok_needed=" libc.musl-$ARCH.so.1 libpq.so.5 libssl.so.3 libcrypto.so.3 "
  for f in $(cd "$ENGINE" && find bin lib -type f); do
    p=$ENGINE/$f
    head -c 4 "$p" | grep -q 'ELF' || continue
    [ "$f" = "lib/ld-musl-$ARCH.so.1" ] && continue
    strip --strip-unneeded "$p"
    for n in $(readelf -d "$p" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'); do
      case "$ok_needed" in *" $n "*) ;; *) echo "  $f needs $n"; bad=1 ;; esac
    done
    # libssl finds libcrypto through the RUNPATH of whoever loaded it (musl
    # searches the requesting objects' RUNPATHs in turn), so it needs none.
    if [ "$f" != lib/libssl.so.3 ] && readelf -d "$p" | grep -q '(NEEDED).*\[lib\(pq\|ssl\|crypto\)'; then
      readelf -d "$p" | grep -q '(RUNPATH).*\[\$ORIGIN/../lib\]' \
        || { echo "  $f: RUNPATH is not \$ORIGIN/../lib: $(readelf -d "$p" | grep -E 'RPATH|RUNPATH')"; bad=1; }
    fi
    case "$f" in
      bin/*) interp=$(readelf -l "$p" | sed -n 's/.*Requesting program interpreter: \(.*\)\]/\1/p')
             [ "$interp" = "$PLACEHOLDER" ] || { echo "  $f: interpreter $interp"; bad=1; } ;;
    esac
  done
  [ $bad -eq 0 ] || exit 1
  readelf -d "$ENGINE/bin/psql" | grep -E 'NEEDED|RUNPATH'
fi

# ---------------------------------------------------------------- pack
log "pack"
NAME=forgebase-pg$PGVER-$BUILD-$TARGET.tar.zst
features=(--feature "pgvector=$(lock sources.pgvector.version)" --feature "pg_cron=$(lock sources.pg_cron.version)"
          --feature "pgmq=$(lock sources.pgmq.version)" --feature "pg_trgm=$PGVER")
case " $EXTENSIONS " in
  *" age "*) features+=(--feature "graph_cypher=true" --feature "age=$(lock sources.age.version)") ;;
  *) features+=(--feature "graph_cypher=false") ;;
esac
loader=()
[ "$OS" = linux ] && loader=(--loader "lib/ld-musl-$ARCH.so.1")
"$PY" "$HERE/engine.py" pack "$ENGINE" "$OUT/$NAME" --postgres "$PGVER" --build "$BUILD" --target "$TARGET" \
  --zstd "$DEPS/bin/zstd" "${loader[@]+"${loader[@]}"}" "${features[@]}" | tee "$OUT/$NAME.pack.txt"
du -sk "$ENGINE" | awk '{printf "engine tree on disk: %d KiB\n", $1}'
ls -l "$OUT/$NAME"
