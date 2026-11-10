//! `PT_INTERP` relocation of Linux engine executables (ADR-0028 §2.3).
//!
//! FB-130 links every Linux executable with
//! `-Wl,--dynamic-linker=/forgebase-pg-loader-<padding to 255 bytes>`. At
//! extraction ForgeBase overwrites that NUL-terminated string, in place, with
//! the absolute path of the extracted musl loader. Libraries are found through
//! `RUNPATH=$ORIGIN/../lib`, so nothing else needs patching.

use std::path::Path;

use object::elf::{FileHeader32, FileHeader64, PT_INTERP};
use object::read::elf::{FileHeader, ProgramHeader};
use object::{Endianness, FileKind};

use crate::error::{EngineError, Result};

/// Placeholder prefix of the build-time interpreter path.
pub const PLACEHOLDER_PREFIX: &str = "/forgebase-pg-loader-";

/// Longest interpreter path that fits the padded segment (255 bytes plus NUL).
pub const MAX_INTERP_LEN: usize = 254;

/// Outcome of [`patch_interp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Patched {
    /// The placeholder was replaced.
    Yes,
    /// The file already names `new` (a re-run).
    AlreadyDone,
    /// Not an ELF file, or an ELF without `PT_INTERP` (a library).
    NotApplicable,
}

/// Byte range of the `PT_INTERP` segment's contents, if any.
fn interp_range(data: &[u8]) -> Result<Option<(usize, usize)>> {
    match FileKind::parse(data) {
        Ok(FileKind::Elf64) => find::<FileHeader64<Endianness>>(data),
        Ok(FileKind::Elf32) => find::<FileHeader32<Endianness>>(data),
        _ => Ok(None),
    }
}

fn find<H: FileHeader<Endian = Endianness>>(data: &[u8]) -> Result<Option<(usize, usize)>> {
    let bad = |e: object::Error| EngineError::Archive(format!("malformed ELF file: {e}"));
    let header = H::parse(data).map_err(bad)?;
    let endian = header.endian().map_err(bad)?;
    for ph in header.program_headers(endian, data).map_err(bad)? {
        if ph.p_type(endian) == PT_INTERP {
            let off: u64 = ph.p_offset(endian).into();
            let size: u64 = ph.p_filesz(endian).into();
            let (off, size) = (
                usize::try_from(off).map_err(|_| EngineError::Archive("PT_INTERP offset too large".into()))?,
                usize::try_from(size).map_err(|_| EngineError::Archive("PT_INTERP size too large".into()))?,
            );
            if off.checked_add(size).is_none_or(|end| end > data.len()) {
                return Err(EngineError::Archive("PT_INTERP segment lies outside the file".into()));
            }
            return Ok(Some((off, size)));
        }
    }
    Ok(None)
}

/// The interpreter an ELF executable names, if any.
///
/// # Errors
/// A malformed ELF file.
pub fn read_interp(data: &[u8]) -> Result<Option<String>> {
    Ok(interp_range(data)?.map(|(off, size)| {
        let seg = &data[off..off + size];
        let end = seg.iter().position(|&b| b == 0).unwrap_or(seg.len());
        String::from_utf8_lossy(&seg[..end]).into_owned()
    }))
}

/// Replaces a placeholder interpreter (starting with `placeholder_prefix`)
/// with `new`, NUL-padding the rest of the segment.
///
/// # Errors
/// A malformed ELF file, an interpreter that is neither the placeholder nor
/// `new`, or a `new` path that does not fit the segment.
pub fn patch_interp(data: &mut [u8], new: &str, placeholder_prefix: &str) -> Result<Patched> {
    let Some((off, size)) = interp_range(data)? else {
        return Ok(Patched::NotApplicable);
    };
    let current = read_interp(data)?.unwrap_or_default();
    if current == new {
        return Ok(Patched::AlreadyDone);
    }
    if !current.starts_with(placeholder_prefix) {
        return Err(EngineError::Archive(format!(
            "interpreter {current:?} is not the relocatable placeholder ({placeholder_prefix}...)"
        )));
    }
    if new.len() + 1 > size || new.as_bytes().contains(&0) {
        return Err(EngineError::Archive(format!(
            "loader path {new:?} is {} bytes, the segment holds {}",
            new.len(),
            size.saturating_sub(1)
        )));
    }
    let seg = &mut data[off..off + size];
    seg.fill(0);
    seg[..new.len()].copy_from_slice(new.as_bytes());
    Ok(Patched::Yes)
}

/// [`patch_interp`] on a file, rewritten in place when changed.
///
/// # Errors
/// I/O errors and those of [`patch_interp`].
pub fn patch_file(path: &Path, new: &str, placeholder_prefix: &str) -> Result<Patched> {
    let mut data = std::fs::read(path).map_err(|e| EngineError::io("read", path, e))?;
    let outcome = patch_interp(&mut data, new, placeholder_prefix)
        .map_err(|e| EngineError::Archive(format!("{}: {e}", path.display())))?;
    if outcome == Patched::Yes {
        std::fs::write(path, &data).map_err(|e| EngineError::io("write", path, e))?;
    }
    Ok(outcome)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal little-endian ELF64 executable whose only program header is
    /// a 256-byte `PT_INTERP` segment holding `interp`.
    pub(crate) fn fake_elf(interp: &str) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
        d.extend_from_slice(&[0; 8]);
        d.extend_from_slice(&2u16.to_le_bytes()); // e_type EXEC
        d.extend_from_slice(&62u16.to_le_bytes()); // e_machine x86_64
        d.extend_from_slice(&1u32.to_le_bytes()); // e_version
        d.extend_from_slice(&0u64.to_le_bytes()); // e_entry
        d.extend_from_slice(&64u64.to_le_bytes()); // e_phoff
        d.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
        d.extend_from_slice(&0u32.to_le_bytes()); // e_flags
        d.extend_from_slice(&64u16.to_le_bytes()); // e_ehsize
        d.extend_from_slice(&56u16.to_le_bytes()); // e_phentsize
        d.extend_from_slice(&1u16.to_le_bytes()); // e_phnum
        d.extend_from_slice(&64u16.to_le_bytes()); // e_shentsize
        d.extend_from_slice(&0u16.to_le_bytes()); // e_shnum
        d.extend_from_slice(&0u16.to_le_bytes()); // e_shstrndx
        assert_eq!(d.len(), 64);
        d.extend_from_slice(&3u32.to_le_bytes()); // p_type PT_INTERP
        d.extend_from_slice(&4u32.to_le_bytes()); // p_flags
        d.extend_from_slice(&120u64.to_le_bytes()); // p_offset
        d.extend_from_slice(&0u64.to_le_bytes()); // p_vaddr
        d.extend_from_slice(&0u64.to_le_bytes()); // p_paddr
        d.extend_from_slice(&256u64.to_le_bytes()); // p_filesz
        d.extend_from_slice(&256u64.to_le_bytes()); // p_memsz
        d.extend_from_slice(&1u64.to_le_bytes()); // p_align
        assert_eq!(d.len(), 120);
        let mut seg = vec![0u8; 256];
        seg[..interp.len()].copy_from_slice(interp.as_bytes());
        d.extend_from_slice(&seg);
        d.extend_from_slice(b"rest of the program");
        d
    }

    pub(crate) fn placeholder() -> String {
        let mut p = PLACEHOLDER_PREFIX.to_owned();
        while p.len() < 255 {
            p.push('x');
        }
        p
    }

    #[test]
    fn patches_the_placeholder_in_place() {
        let mut elf = fake_elf(&placeholder());
        let len = elf.len();
        let new = "/home/u/project/postgres/engine/18.6-r1-abc/lib/ld-musl-x86_64.so.1";
        assert_eq!(
            patch_interp(&mut elf, new, PLACEHOLDER_PREFIX).expect("patch"),
            Patched::Yes
        );
        assert_eq!(elf.len(), len);
        assert_eq!(read_interp(&elf).expect("read").as_deref(), Some(new));
        assert!(elf[120 + new.len()..120 + 256].iter().all(|&b| b == 0));
        assert!(elf.ends_with(b"rest of the program"));
        assert_eq!(
            patch_interp(&mut elf, new, PLACEHOLDER_PREFIX).expect("again"),
            Patched::AlreadyDone
        );
    }

    #[test]
    fn refuses_foreign_interpreters_and_long_paths() {
        let mut elf = fake_elf("/lib/ld-linux-x86-64.so.2");
        assert!(patch_interp(&mut elf, "/x/ld.so", PLACEHOLDER_PREFIX).is_err());
        let mut elf = fake_elf(&placeholder());
        let long = format!("/{}", "a".repeat(255));
        assert!(patch_interp(&mut elf, &long, PLACEHOLDER_PREFIX).is_err());
        let fits = format!("/{}", "a".repeat(MAX_INTERP_LEN - 1));
        assert_eq!(
            patch_interp(&mut elf, &fits, PLACEHOLDER_PREFIX).expect("fits"),
            Patched::Yes
        );
    }

    #[test]
    fn non_elf_and_truncated_files() {
        let mut text = b"#!/bin/sh\necho hi\n".to_vec();
        assert_eq!(
            patch_interp(&mut text, "/x", PLACEHOLDER_PREFIX).expect("not elf"),
            Patched::NotApplicable
        );
        let mut elf = fake_elf(&placeholder());
        elf.truncate(200);
        assert!(patch_interp(&mut elf, "/x", PLACEHOLDER_PREFIX).is_err());
    }

    #[test]
    fn patch_file_rewrites_only_when_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("postgres");
        std::fs::write(&p, fake_elf(&placeholder())).expect("write");
        assert_eq!(
            patch_file(&p, "/a/ld.so", PLACEHOLDER_PREFIX).expect("patch"),
            Patched::Yes
        );
        let data = std::fs::read(&p).expect("read");
        assert_eq!(read_interp(&data).expect("interp").as_deref(), Some("/a/ld.so"));
    }
}
