#!/usr/bin/env python3
"""Extract a source tarball: untar.py <archive> <dest-dir>.

build-windows.ps1 uses it for .tar.bz2: Windows' tar.exe starts an external
`bzip2 -d` for those and the pair deadlocked on the GitHub runner
(engine-pg18.6-r2, the PostgreSQL source, no output for 10 minutes). Python's
tarfile decompresses bz2 itself. The archive has already been checked against
the SHA-256 in engine.lock, so the extraction filter is "fully_trusted".
"""
import sys
import tarfile

if len(sys.argv) != 3:
    sys.exit("usage: untar.py <archive> <dest-dir>")
with tarfile.open(sys.argv[1]) as tf:
    tf.extractall(sys.argv[2], filter="fully_trusted")
