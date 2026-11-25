//! Minimal PE reader: lists the DLL names in the import and delay-import tables, and reads the
//! embedded application manifest.
//!
//! This is the fallback (and cross-check) for `dumpbin /dependents`, so the packaging check
//! works on machines without the MSVC Build Tools.

use anyhow::{anyhow, bail, Result};

/// DLL names a PE image depends on, in table order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Imports {
    /// Regular import table (data directory 1).
    pub regular: Vec<String>,
    /// Delay-load import table (data directory 13).
    pub delayed: Vec<String>,
}

impl Imports {
    /// Regular then delayed names, without duplicates (compared case-insensitively).
    pub fn all(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.regular.iter().chain(&self.delayed) {
            if !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                out.push(name.clone());
            }
        }
        out
    }
}

const DIR_IMPORT: usize = 1;
const DIR_DELAY_IMPORT: usize = 13;
/// Upper bound on descriptors read from one table, so a damaged file cannot loop forever.
const MAX_DESCRIPTORS: usize = 4096;

fn bytes(data: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    off.checked_add(len)
        .and_then(|end| data.get(off..end))
        .ok_or_else(|| anyhow!("PE image is truncated (wanted {len} bytes at offset {off:#x})"))
}

fn u16_at(data: &[u8], off: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(data, off, 2)?.try_into().unwrap()))
}

fn u32_at(data: &[u8], off: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, off, 4)?.try_into().unwrap()))
}

fn u64_at(data: &[u8], off: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, off, 8)?.try_into().unwrap()))
}

struct Section {
    virtual_address: u32,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

struct Image<'a> {
    data: &'a [u8],
    image_base: u64,
    sections: Vec<Section>,
    /// (rva, size) per data directory.
    directories: Vec<(u32, u32)>,
}

impl<'a> Image<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        if bytes(data, 0, 2)? != b"MZ" {
            bail!("not a PE image: no MZ header");
        }
        let pe = u32_at(data, 0x3c)? as usize;
        if bytes(data, pe, 4)? != b"PE\0\0" {
            bail!("not a PE image: no PE signature");
        }
        let coff = pe + 4;
        let section_count = u16_at(data, coff + 2)? as usize;
        let optional_size = u16_at(data, coff + 16)? as usize;
        let opt = coff + 20;
        let (image_base, dir_count_off) = match u16_at(data, opt)? {
            0x10b => (u64::from(u32_at(data, opt + 28)?), opt + 92),
            0x20b => (u64_at(data, opt + 24)?, opt + 108),
            other => bail!("unknown PE optional-header magic {other:#x}"),
        };
        let dir_count = (u32_at(data, dir_count_off)? as usize).min(16);
        let mut directories = Vec::with_capacity(dir_count);
        for i in 0..dir_count {
            let at = dir_count_off + 4 + i * 8;
            directories.push((u32_at(data, at)?, u32_at(data, at + 4)?));
        }
        let table = opt + optional_size;
        let mut sections = Vec::with_capacity(section_count);
        for i in 0..section_count {
            let at = table + i * 40;
            sections.push(Section {
                virtual_size: u32_at(data, at + 8)?,
                virtual_address: u32_at(data, at + 12)?,
                raw_size: u32_at(data, at + 16)?,
                raw_offset: u32_at(data, at + 20)?,
            });
        }
        Ok(Image {
            data,
            image_base,
            sections,
            directories,
        })
    }

    /// File offset of a relative virtual address.
    fn offset(&self, rva: u32) -> Result<usize> {
        for s in &self.sections {
            let span = s.virtual_size.max(s.raw_size);
            if rva >= s.virtual_address && rva - s.virtual_address < span {
                let delta = rva - s.virtual_address;
                if delta >= s.raw_size {
                    bail!("RVA {rva:#x} lies in the zero-filled tail of its section");
                }
                return Ok((s.raw_offset + delta) as usize);
            }
        }
        bail!("RVA {rva:#x} is not inside any section")
    }

    fn name_at(&self, rva: u32) -> Result<String> {
        let start = self.offset(rva)?;
        let tail = self
            .data
            .get(start..)
            .ok_or_else(|| anyhow!("DLL name at RVA {rva:#x} is past the end of the file"))?;
        let len = tail
            .iter()
            .take(260)
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("unterminated DLL name at RVA {rva:#x}"))?;
        let name = &tail[..len];
        if name.is_empty() || !name.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            bail!("DLL name at RVA {rva:#x} is not printable ASCII");
        }
        Ok(String::from_utf8_lossy(name).into_owned())
    }

    fn directory(&self, index: usize) -> Option<u32> {
        match self.directories.get(index) {
            Some(&(rva, size)) if rva != 0 && size != 0 => Some(rva),
            _ => None,
        }
    }

    fn regular(&self) -> Result<Vec<String>> {
        let Some(rva) = self.directory(DIR_IMPORT) else {
            return Ok(Vec::new());
        };
        let base = self.offset(rva)?;
        let mut names = Vec::new();
        for i in 0..MAX_DESCRIPTORS {
            // IMAGE_IMPORT_DESCRIPTOR is 20 bytes; an all-zero entry ends the table.
            let entry = bytes(self.data, base + i * 20, 20)?;
            if entry.iter().all(|&b| b == 0) {
                return Ok(names);
            }
            names.push(self.name_at(u32_at(entry, 12)?)?);
        }
        bail!("import table has no terminator")
    }

    fn delayed(&self) -> Result<Vec<String>> {
        let Some(rva) = self.directory(DIR_DELAY_IMPORT) else {
            return Ok(Vec::new());
        };
        let base = self.offset(rva)?;
        let mut names = Vec::new();
        for i in 0..MAX_DESCRIPTORS {
            // ImgDelayDescr is 32 bytes: attributes, then the DLL name address.
            let entry = bytes(self.data, base + i * 32, 32)?;
            let attributes = u32_at(entry, 0)?;
            let mut name = u32_at(entry, 4)?;
            if name == 0 {
                return Ok(names);
            }
            if attributes & 1 == 0 {
                // Old-style descriptor: fields are virtual addresses, not RVAs.
                name = u64::from(name)
                    .checked_sub(self.image_base)
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| anyhow!("delay-import name address is below the image base"))?;
            }
            names.push(self.name_at(name)?);
        }
        bail!("delay-import table has no terminator")
    }
}

/// Reads the DLL dependency names out of a PE32 or PE32+ image.
pub fn imports(data: &[u8]) -> Result<Imports> {
    let image = Image::parse(data)?;
    Ok(Imports {
        regular: image.regular()?,
        delayed: image.delayed()?,
    })
}

const DIR_RESOURCE: usize = 2;
/// Resource type of the application manifest.
const RT_MANIFEST: u32 = 24;
/// Upper bound on the entries read from one resource directory.
const MAX_RESOURCE_ENTRIES: usize = 4096;

/// The `(id or name, offset)` entries of the resource directory at `at` (an offset into the
/// resource section, whose file offset is `base`).
fn resource_entries(data: &[u8], base: usize, at: u32) -> Result<Vec<(u32, u32)>> {
    let dir = base + at as usize;
    let count = u16_at(data, dir + 12)? as usize + u16_at(data, dir + 14)? as usize;
    if count > MAX_RESOURCE_ENTRIES {
        bail!("resource directory claims {count} entries");
    }
    (0..count)
        .map(|i| {
            let at = dir + 16 + i * 8;
            Ok((u32_at(data, at)?, u32_at(data, at + 4)?))
        })
        .collect()
}

/// The directory an entry's offset points to, or an error when it points to data instead.
fn subdirectory(offset: u32) -> Result<u32> {
    if offset & 0x8000_0000 == 0 {
        bail!("resource tree has a data entry where a directory belongs");
    }
    Ok(offset & 0x7fff_ffff)
}

/// The bytes of the application manifest (resource type `RT_MANIFEST`, the first name and
/// language under it), or `None` when the image has no manifest resource.
pub fn manifest(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let image = Image::parse(data)?;
    let Some(rva) = image.directory(DIR_RESOURCE) else {
        return Ok(None);
    };
    let base = image.offset(rva)?;
    let Some(&(_, types)) = resource_entries(data, base, 0)?
        .iter()
        .find(|(id, _)| *id == RT_MANIFEST)
    else {
        return Ok(None);
    };
    let names = resource_entries(data, base, subdirectory(types)?)?;
    let Some(&(_, name)) = names.first() else {
        return Ok(None);
    };
    let languages = resource_entries(data, base, subdirectory(name)?)?;
    let Some(&(_, entry)) = languages.first() else {
        return Ok(None);
    };
    if entry & 0x8000_0000 != 0 {
        bail!("resource tree is deeper than type, name and language");
    }
    let entry = base + entry as usize;
    let (data_rva, size) = (u32_at(data, entry)?, u32_at(data, entry + 4)?);
    let manifest = bytes(data, image.offset(data_rva)?, size as usize)?;
    Ok(Some(manifest.to_vec()))
}

#[cfg(test)]
pub mod synth {
    //! Builds a tiny PE image with chosen import tables, for tests only.

    const SECTION_RVA: u32 = 0x1000;
    const SECTION_RAW: u32 = 0x200;

    pub struct Spec<'a> {
        pub pe32_plus: bool,
        pub regular: &'a [&'a str],
        pub delayed: &'a [&'a str],
        /// Delay descriptors hold RVAs (modern) rather than virtual addresses (old style).
        pub delay_rva: bool,
    }

    pub fn build(spec: &Spec) -> Vec<u8> {
        let image_base: u64 = if spec.pe32_plus { 0x1_8000_0000 } else { 0x1000_0000 };

        // Section body: import descriptors, delay descriptors, then the name strings.
        let import_len = (spec.regular.len() + 1) * 20;
        let delay_len = (spec.delayed.len() + 1) * 32;
        let mut names = Vec::new();
        let mut name_rvas = Vec::new();
        for name in spec.regular.iter().chain(spec.delayed) {
            name_rvas.push(SECTION_RVA + (import_len + delay_len + names.len()) as u32);
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        let mut body = Vec::new();
        for rva in &name_rvas[..spec.regular.len()] {
            let mut d = [0u8; 20];
            d[0..4].copy_from_slice(&0xdead_u32.to_le_bytes()); // non-zero lookup table
            d[12..16].copy_from_slice(&rva.to_le_bytes());
            body.extend_from_slice(&d);
        }
        body.extend_from_slice(&[0u8; 20]);
        for rva in &name_rvas[spec.regular.len()..] {
            let mut d = [0u8; 32];
            let (attrs, addr) = if spec.delay_rva {
                (1u32, *rva)
            } else {
                (0u32, (image_base + u64::from(*rva)) as u32)
            };
            d[0..4].copy_from_slice(&attrs.to_le_bytes());
            d[4..8].copy_from_slice(&addr.to_le_bytes());
            body.extend_from_slice(&d);
        }
        body.extend_from_slice(&[0u8; 32]);
        body.extend_from_slice(&names);

        let opt_size: usize = if spec.pe32_plus { 112 + 16 * 8 } else { 96 + 16 * 8 };
        let mut out = vec![0u8; 0x40];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        out.extend_from_slice(b"PE\0\0");
        let mut coff = [0u8; 20];
        coff[2..4].copy_from_slice(&1u16.to_le_bytes());
        coff[16..18].copy_from_slice(&(opt_size as u16).to_le_bytes());
        out.extend_from_slice(&coff);

        let mut opt = vec![0u8; opt_size];
        let dirs = if spec.pe32_plus {
            opt[0..2].copy_from_slice(&0x20bu16.to_le_bytes());
            opt[24..32].copy_from_slice(&image_base.to_le_bytes());
            108
        } else {
            opt[0..2].copy_from_slice(&0x10bu16.to_le_bytes());
            opt[28..32].copy_from_slice(&(image_base as u32).to_le_bytes());
            92
        };
        opt[dirs..dirs + 4].copy_from_slice(&16u32.to_le_bytes());
        let mut set_dir = |index: usize, rva: u32, size: u32| {
            let at = dirs + 4 + index * 8;
            opt[at..at + 4].copy_from_slice(&rva.to_le_bytes());
            opt[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
        };
        if !spec.regular.is_empty() {
            set_dir(1, SECTION_RVA, import_len as u32);
        }
        if !spec.delayed.is_empty() {
            set_dir(13, SECTION_RVA + import_len as u32, delay_len as u32);
        }
        out.extend_from_slice(&opt);

        let mut section = [0u8; 40];
        section[0..6].copy_from_slice(b".rdata");
        section[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
        section[12..16].copy_from_slice(&SECTION_RVA.to_le_bytes());
        section[16..20].copy_from_slice(&(body.len() as u32).to_le_bytes());
        section[20..24].copy_from_slice(&SECTION_RAW.to_le_bytes());
        out.extend_from_slice(&section);

        assert!(out.len() <= SECTION_RAW as usize);
        out.resize(SECTION_RAW as usize, 0);
        out.extend_from_slice(&body);
        out
    }
}

#[cfg(test)]
pub mod synth_resources {
    //! Adds a resource section holding one `RT_MANIFEST` to a synthetic image, for tests only.

    use super::synth::{build, Spec};

    fn dir(out: &mut Vec<u8>, entries: &[(u32, u32)]) {
        let mut header = [0u8; 16];
        header[14..16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&header);
        for (id, offset) in entries {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
        }
    }

    /// A PE32+ image whose only section is a resource tree with an icon directory and, when
    /// `manifest` is given, an `RT_MANIFEST` holding those bytes.
    pub fn with_manifest(manifest: Option<&[u8]>) -> Vec<u8> {
        const RVA: u32 = 0x1000;
        const RAW: usize = 0x200;
        let mut image = build(&Spec {
            pe32_plus: true,
            regular: &[],
            delayed: &[],
            delay_rva: true,
        });
        image.truncate(RAW);
        // Root (2 entries, 32 bytes) at 0, type dir at 0x20, name dir at 0x38, data entry at
        // 0x50, data at 0x60. The icon type (3) shares the type directory; it is never read.
        let mut body = Vec::new();
        let mut root = vec![(3u32, 0x8000_0020u32)];
        if manifest.is_some() {
            root.push((24, 0x8000_0020));
        }
        dir(&mut body, &root);
        body.resize(0x20, 0);
        dir(&mut body, &[(1, 0x8000_0038)]);
        dir(&mut body, &[(0x409, 0x50)]);
        let bytes = manifest.unwrap_or_default();
        body.extend_from_slice(&(RVA + 0x60).to_le_bytes());
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(bytes);

        // Point data directory 2 at the section and give the section the new size.
        let opt = 0x40 + 4 + 20;
        let dir2 = opt + 108 + 4 + 2 * 8;
        image[dir2..dir2 + 4].copy_from_slice(&RVA.to_le_bytes());
        image[dir2 + 4..dir2 + 8].copy_from_slice(&(body.len() as u32).to_le_bytes());
        let section = opt + 112 + 16 * 8;
        let len = (body.len() as u32).to_le_bytes();
        image[section + 8..section + 12].copy_from_slice(&len);
        image[section + 16..section + 20].copy_from_slice(&len);
        image.extend_from_slice(&body);
        image
    }
}

#[cfg(test)]
mod tests {
    use super::synth::{build, Spec};
    use super::*;

    #[test]
    fn reads_regular_imports_from_pe32_plus() {
        let image = build(&Spec {
            pe32_plus: true,
            regular: &["KERNEL32.dll", "USER32.dll", "api-ms-win-crt-heap-l1-1-0.dll"],
            delayed: &[],
            delay_rva: true,
        });
        let got = imports(&image).unwrap();
        assert_eq!(
            got.regular,
            ["KERNEL32.dll", "USER32.dll", "api-ms-win-crt-heap-l1-1-0.dll"]
        );
        assert!(got.delayed.is_empty());
    }

    #[test]
    fn reads_regular_imports_from_pe32() {
        let image = build(&Spec {
            pe32_plus: false,
            regular: &["GDI32.dll"],
            delayed: &[],
            delay_rva: true,
        });
        assert_eq!(imports(&image).unwrap().regular, ["GDI32.dll"]);
    }

    #[test]
    fn reads_delay_imports_in_both_address_styles() {
        for (pe32_plus, delay_rva) in [(true, true), (false, true), (false, false)] {
            let image = build(&Spec {
                pe32_plus,
                regular: &["KERNEL32.dll"],
                delayed: &["VCRUNTIME140.dll", "dbghelp.dll"],
                delay_rva,
            });
            let got = imports(&image).unwrap();
            assert_eq!(got.regular, ["KERNEL32.dll"]);
            assert_eq!(got.delayed, ["VCRUNTIME140.dll", "dbghelp.dll"]);
        }
    }

    #[test]
    fn all_merges_without_case_duplicates() {
        let got = Imports {
            regular: vec!["KERNEL32.dll".into(), "USER32.dll".into()],
            delayed: vec!["kernel32.DLL".into(), "ole32.dll".into()],
        };
        assert_eq!(got.all(), ["KERNEL32.dll", "USER32.dll", "ole32.dll"]);
    }

    #[test]
    fn image_without_import_tables_has_no_imports() {
        let image = build(&Spec {
            pe32_plus: true,
            regular: &[],
            delayed: &[],
            delay_rva: true,
        });
        assert_eq!(imports(&image).unwrap(), Imports::default());
    }

    #[test]
    fn rejects_non_pe_input() {
        assert!(imports(b"").is_err());
        assert!(imports(b"%PDF-1.7 not a dll").is_err());
        let mut image = build(&Spec {
            pe32_plus: true,
            regular: &["KERNEL32.dll"],
            delayed: &[],
            delay_rva: true,
        });
        image[0x40] = b'X';
        assert!(imports(&image).is_err());
    }

    #[test]
    fn truncated_image_is_an_error_not_a_panic() {
        let image = build(&Spec {
            pe32_plus: true,
            regular: &["KERNEL32.dll", "USER32.dll"],
            delayed: &["dbghelp.dll"],
            delay_rva: true,
        });
        let full = imports(&image).unwrap();
        for len in 0..image.len() {
            // Every prefix must either fail cleanly or (never) equal the full answer early.
            if let Ok(partial) = imports(&image[..len]) {
                assert_eq!(partial, full, "prefix of {len} bytes gave a different answer");
            }
        }
    }

    #[test]
    fn reads_the_manifest_resource() {
        let text = b"<assembly><requestedExecutionLevel level='asInvoker'/></assembly>";
        let image = super::synth_resources::with_manifest(Some(text));
        assert_eq!(manifest(&image).unwrap().as_deref(), Some(&text[..]));
        // Without the RT_MANIFEST entry, or without a resource section at all: none.
        let image = super::synth_resources::with_manifest(None);
        assert_eq!(manifest(&image).unwrap(), None);
        let plain = build(&Spec {
            pe32_plus: true,
            regular: &["KERNEL32.dll"],
            delayed: &[],
            delay_rva: true,
        });
        assert_eq!(manifest(&plain).unwrap(), None);
    }

    #[test]
    fn truncated_resources_are_an_error_not_a_panic() {
        let image = super::synth_resources::with_manifest(Some(b"<assembly/>"));
        for len in 0..image.len() {
            let _ = manifest(&image[..len]);
        }
    }
}
