//! Windows shortcuts and icons, read by armor itself (ADR-0040): the `.lnk`
//! files an installer leaves in a prefix's Start Menu become menu entries,
//! with the icon taken from the program (its PE resources, written out as
//! an `.ico`). Wine's own menu builder does this for Hangover's prefixes;
//! Proton does not run it.

use std::fs;
use std::path::{Path, PathBuf};

/// What a `.lnk` file says (MS-SHLLINK), as far as a menu entry needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shortcut {
    /// The target's Windows path (`C:\Program Files\...\x.exe`).
    pub target: Option<String>,
    pub arguments: Option<String>,
    pub working_dir: Option<String>,
    /// The icon's file (Windows path) and index, when the shortcut names
    /// one; else the target's first icon is meant.
    pub icon_location: Option<String>,
    pub icon_index: i32,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// A NUL-terminated string at `at` (ANSI, read as Latin-1).
fn c_string(data: &[u8], at: usize) -> Option<String> {
    let rest = data.get(at..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    Some(rest[..end].iter().map(|b| char::from(*b)).collect())
}

/// Reads a shell link (`.lnk`); `None` if it is not one.
pub fn parse_lnk(data: &[u8]) -> Option<Shortcut> {
    const CLSID: [u8; 16] = [
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ];
    if u32_at(data, 0)? != 0x4c || data.get(4..20)? != CLSID {
        return None;
    }
    let flags = u32_at(data, 0x14)?;
    let icon_index = u32_at(data, 0x38)? as i32;
    let has = |bit: u32| flags & bit != 0;
    let unicode = has(0x80);
    let mut at = 0x4c;
    let mut shortcut = Shortcut {
        icon_index,
        ..Shortcut::default()
    };
    if has(0x01) {
        // LinkTargetIDList: skipped (LinkInfo gives the path).
        at += 2 + usize::from(u16_at(data, at)?);
    }
    if has(0x02) {
        let size = u32_at(data, at)? as usize;
        let header = u32_at(data, at + 4)? as usize;
        let info_flags = u32_at(data, at + 8)?;
        if info_flags & 1 != 0 {
            let base = u32_at(data, at + 16)? as usize;
            let suffix = u32_at(data, at + 24)? as usize;
            let mut path = c_string(data, at + base)?;
            if suffix != 0 {
                path.push_str(&c_string(data, at + suffix).unwrap_or_default());
            }
            // The Unicode form, when the header is long enough to name it.
            if header >= 0x24
                && let Some(base_unicode) = u32_at(data, at + 28).filter(|o| *o != 0)
            {
                let mut units = Vec::new();
                let mut p = at + base_unicode as usize;
                while let Some(unit) = u16_at(data, p).filter(|u| *u != 0) {
                    units.push(unit);
                    p += 2;
                }
                path = String::from_utf16_lossy(&units);
            }
            shortcut.target = Some(path);
        }
        at += size;
    }
    // StringData, in this order, each present by its flag.
    let read_string = |at: &mut usize| -> Option<String> {
        let count = usize::from(u16_at(data, *at)?);
        *at += 2;
        let text = if unicode {
            let units: Vec<u16> = (0..count)
                .map(|i| u16_at(data, *at + 2 * i))
                .collect::<Option<_>>()?;
            *at += 2 * count;
            String::from_utf16_lossy(&units)
        } else {
            let bytes = data.get(*at..*at + count)?;
            *at += count;
            bytes.iter().map(|b| char::from(*b)).collect()
        };
        Some(text)
    };
    let mut strings = [None, None, None, None, None];
    for (i, bit) in [0x04, 0x08, 0x10, 0x20, 0x40].into_iter().enumerate() {
        if has(bit) {
            strings[i] = Some(read_string(&mut at)?);
        }
    }
    let [_name, relative, working_dir, arguments, icon] = strings;
    if shortcut.target.is_none() {
        shortcut.target = relative;
    }
    shortcut.working_dir = working_dir;
    shortcut.arguments = arguments.filter(|a| !a.is_empty());
    shortcut.icon_location = icon.filter(|i| !i.is_empty());
    Some(shortcut)
}

/// The host path of a Windows path inside the prefix (`C:\...` →
/// `<prefix>/drive_c/...`, case of the directories as they are on disk).
pub fn host_path(prefix: &Path, windows: &str) -> Option<PathBuf> {
    let windows = windows.trim().trim_matches('"');
    let rest = windows
        .strip_prefix("C:\\")
        .or_else(|| windows.strip_prefix("c:\\"))?;
    let mut path = prefix.join("drive_c");
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        // Windows paths ignore case.
        let found = fs::read_dir(&path).ok().and_then(|read| {
            read.flatten().map(|e| e.path()).find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(part))
            })
        });
        path = found.unwrap_or(exact);
    }
    Some(path)
}

/// The PE file's resources: the file offset of the resource directory and
/// a function from RVA to file offset.
struct Resources<'a> {
    data: &'a [u8],
    base: usize,
    sections: Vec<(u32, u32, u32)>,
}

impl Resources<'_> {
    fn new(data: &[u8]) -> Option<Resources<'_>> {
        if data.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(data, 0x3c)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let sections_count = usize::from(u16_at(data, pe + 6)?);
        let optional_size = usize::from(u16_at(data, pe + 20)?);
        let optional = pe + 24;
        let directories = match u16_at(data, optional)? {
            0x10b => optional + 96,
            0x20b => optional + 112,
            _ => return None,
        };
        let resource_rva = u32_at(data, directories + 2 * 8)?;
        if resource_rva == 0 {
            return None;
        }
        let table = optional + optional_size;
        let sections: Vec<(u32, u32, u32)> = (0..sections_count)
            .filter_map(|i| {
                let s = table + 40 * i;
                Some((
                    u32_at(data, s + 12)?,
                    u32_at(data, s + 16)?,
                    u32_at(data, s + 20)?,
                ))
            })
            .collect();
        let mut resources = Resources {
            data,
            base: 0,
            sections,
        };
        resources.base = resources.offset(resource_rva)?;
        Some(resources)
    }

    /// The file offset of `rva`.
    fn offset(&self, rva: u32) -> Option<usize> {
        self.sections
            .iter()
            .find(|(va, size, _)| rva >= *va && rva < va + size)
            .map(|(va, _, raw)| (rva - va + raw) as usize)
    }

    /// The entries of the directory at `dir` (offset from the resource
    /// base): (id or name offset, is a name, target, is a subdirectory).
    fn entries(&self, dir: usize) -> Vec<(u32, bool, usize, bool)> {
        let at = self.base + dir;
        let (Some(named), Some(ids)) = (u16_at(self.data, at + 12), u16_at(self.data, at + 14))
        else {
            return Vec::new();
        };
        (0..usize::from(named) + usize::from(ids))
            .filter_map(|i| {
                let e = at + 16 + 8 * i;
                let name = u32_at(self.data, e)?;
                let target = u32_at(self.data, e + 4)?;
                Some((
                    name & 0x7fff_ffff,
                    name & 0x8000_0000 != 0,
                    (target & 0x7fff_ffff) as usize,
                    target & 0x8000_0000 != 0,
                ))
            })
            .collect()
    }

    /// The data of the first language of resource `kind`/entry, where
    /// `pick` chooses the entry among the type's entries.
    fn data_of(&self, entry_dir: usize) -> Option<&[u8]> {
        let (_, _, mut target, mut is_dir) = *self.entries(entry_dir).first()?;
        while is_dir {
            (_, _, target, is_dir) = *self.entries(target).first()?;
        }
        let leaf = self.base + target;
        let rva = u32_at(self.data, leaf)?;
        let size = u32_at(self.data, leaf + 4)? as usize;
        let start = self.offset(rva)?;
        self.data.get(start..start + size)
    }

    /// The subdirectory of type `kind` (an integer resource type).
    fn kind(&self, kind: u32) -> Option<usize> {
        self.entries(0)
            .into_iter()
            .find(|(id, named, _, dir)| !named && *id == kind && *dir)
            .map(|(_, _, target, _)| target)
    }
}

/// The icon group `index` of a PE file (`RT_GROUP_ICON`, in resource
/// order; a negative index is a resource id) as the bytes of an `.ico`.
pub fn pe_icon(data: &[u8], index: i32) -> Option<Vec<u8>> {
    const RT_ICON: u32 = 3;
    const RT_GROUP_ICON: u32 = 14;
    let resources = Resources::new(data)?;
    let groups = resources.entries(resources.kind(RT_GROUP_ICON)?);
    let group = if index < 0 {
        groups
            .iter()
            .find(|(id, named, _, _)| !named && i64::from(*id) == -i64::from(index))?
    } else {
        groups
            .get(usize::try_from(index).ok()?)
            .or(groups.first())?
    };
    let (_, _, group_dir, true) = *group else {
        return None;
    };
    let directory = resources.data_of(group_dir)?;
    let icons = resources.entries(resources.kind(RT_ICON)?);
    let count = usize::from(u16_at(directory, 4)?);
    let mut images: Vec<(&[u8], &[u8])> = Vec::new();
    for i in 0..count {
        // GRPICONDIRENTRY: 12 bytes like an ICONDIRENTRY, then a u16 id.
        let entry = directory.get(6 + 14 * i..6 + 14 * i + 14)?;
        let id = u32::from(u16_at(entry, 12)?);
        let Some((_, _, dir, true)) = icons
            .iter()
            .find(|(i, named, _, _)| !named && *i == id)
            .copied()
        else {
            continue;
        };
        if let Some(image) = resources.data_of(dir) {
            images.push((&entry[..12], image));
        }
    }
    Some(ico_file(&images))
}

/// An `.ico` file from its entries (the first 12 bytes of each
/// ICONDIRENTRY) and their images.
pub fn ico_file(images: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut ico = vec![0, 0, 1, 0];
    ico.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (entry, image) in images {
        ico.extend(&entry[..8]);
        ico.extend((image.len() as u32).to_le_bytes());
        ico.extend((offset as u32).to_le_bytes());
        offset += image.len();
    }
    for (_, image) in images {
        ico.extend(*image);
    }
    ico
}

/// The `.lnk` files under the prefix's Start Menus (all users' and each
/// user's), as host paths, sorted.
pub fn start_menu_links(prefix: &Path) -> Vec<PathBuf> {
    let drive = prefix.join("drive_c");
    let mut menus = vec![drive.join("ProgramData/Microsoft/Windows/Start Menu/Programs")];
    if let Ok(users) = fs::read_dir(drive.join("users")) {
        for user in users.flatten() {
            menus.push(
                user.path()
                    .join("AppData/Roaming/Microsoft/Windows/Start Menu/Programs"),
            );
        }
    }
    let mut links = Vec::new();
    while let Some(dir) = menus.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let path = item.path();
            if path.is_dir() {
                menus.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
            {
                links.push(path);
            }
        }
    }
    links.sort();
    links
}

/// The Windows path (`C:\...`) of a host path inside the prefix.
pub fn windows_path(prefix: &Path, host: &Path) -> Option<String> {
    let rest = host.strip_prefix(prefix.join("drive_c")).ok()?;
    Some(format!("C:\\{}", rest.to_string_lossy().replace('/', "\\")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.lnk` with LinkInfo (a local base path), a working directory,
    /// arguments and an icon location, as Wine and Windows write them.
    fn sample_lnk() -> Vec<u8> {
        let mut lnk = vec![0u8; 0x4c];
        lnk[0] = 0x4c;
        lnk[4..20].copy_from_slice(&[
            0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x46,
        ]);
        // LinkInfo, WorkingDir, Arguments, IconLocation, Unicode.
        let flags: u32 = 0x02 | 0x10 | 0x20 | 0x40 | 0x80;
        lnk[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        lnk[0x38..0x3c].copy_from_slice(&2u32.to_le_bytes());
        let path = b"C:\\Program Files\\App\\app.exe\0";
        let header = 0x1c;
        let mut info = vec![0u8; header];
        let size = header + path.len() + 1;
        info[0..4].copy_from_slice(&(size as u32).to_le_bytes());
        info[4..8].copy_from_slice(&(header as u32).to_le_bytes());
        info[8..12].copy_from_slice(&1u32.to_le_bytes());
        info[16..20].copy_from_slice(&(header as u32).to_le_bytes());
        info[24..28].copy_from_slice(&((header + path.len()) as u32).to_le_bytes());
        info.extend(path);
        info.push(0);
        lnk.extend(info);
        for text in [
            "C:\\Program Files\\App",
            "--fast",
            "C:\\Program Files\\App\\app.ico",
        ] {
            lnk.extend((text.len() as u16).to_le_bytes());
            for unit in text.encode_utf16() {
                lnk.extend(unit.to_le_bytes());
            }
        }
        lnk
    }

    #[test]
    fn shortcuts_are_read() {
        assert_eq!(
            parse_lnk(&sample_lnk()),
            Some(Shortcut {
                target: Some("C:\\Program Files\\App\\app.exe".to_string()),
                arguments: Some("--fast".to_string()),
                working_dir: Some("C:\\Program Files\\App".to_string()),
                icon_location: Some("C:\\Program Files\\App\\app.ico".to_string()),
                icon_index: 2,
            })
        );
        assert_eq!(parse_lnk(b"not a link"), None);
    }

    /// A PE32+ file with one section holding a resource tree: one icon
    /// group (id 1) of one icon (id 7).
    fn sample_pe(icon: &[u8]) -> Vec<u8> {
        let mut pe = vec![0u8; 0x200];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        pe[0x46..0x48].copy_from_slice(&1u16.to_le_bytes());
        let optional_size: u16 = 240;
        pe[0x54..0x56].copy_from_slice(&optional_size.to_le_bytes());
        let optional = 0x58;
        pe[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        let rva: u32 = 0x1000;
        let directories = optional + 112;
        pe[directories + 16..directories + 20].copy_from_slice(&rva.to_le_bytes());
        // Section table entry: VirtualAddress, SizeOfRawData, PointerToRawData.
        let section = optional + usize::from(optional_size);
        let raw: u32 = 0x200;
        // The resource section, built below.
        let mut rsrc = vec![0u8; 0];
        let dir = |entries: &[(u32, u32)]| {
            let mut d = vec![0u8; 16];
            d[14..16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
            for (name, target) in entries {
                d.extend(name.to_le_bytes());
                d.extend(target.to_le_bytes());
            }
            d
        };
        // Layout: root (2 types) at 0; type 3 at 0x20; type 14 at 0x40;
        // id dirs at 0x60 (icon 7) and 0x80 (group 1); language dirs at
        // 0xa0 and 0xc0; leaves at 0xe0 and 0xf0; data at 0x100.
        let sub = 0x8000_0000u32;
        let group = {
            let mut g = vec![0, 0, 1, 0, 1, 0];
            g.extend([16, 16, 0, 0, 1, 0, 32, 0]);
            g.extend((icon.len() as u32).to_le_bytes());
            g.extend(7u16.to_le_bytes());
            g
        };
        let parts: Vec<(usize, Vec<u8>)> = vec![
            (0x00, dir(&[(3, sub | 0x20), (14, sub | 0x40)])),
            (0x20, dir(&[(7, sub | 0x60)])),
            (0x40, dir(&[(1, sub | 0x80)])),
            (0x60, dir(&[(0x409, 0xa0)])),
            (0x80, dir(&[(0x409, 0xb0)])),
            (0xa0, {
                let mut l = (rva + 0x100).to_le_bytes().to_vec();
                l.extend((icon.len() as u32).to_le_bytes());
                l.extend([0u8; 8]);
                l
            }),
            (0xb0, {
                let mut l = (rva + 0x140).to_le_bytes().to_vec();
                l.extend((group.len() as u32).to_le_bytes());
                l.extend([0u8; 8]);
                l
            }),
            (0x100, icon.to_vec()),
            (0x140, group),
        ];
        for (at, bytes) in parts {
            if rsrc.len() < at + bytes.len() {
                rsrc.resize(at + bytes.len(), 0);
            }
            rsrc[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        pe[section + 12..section + 16].copy_from_slice(&rva.to_le_bytes());
        pe[section + 16..section + 20].copy_from_slice(&(rsrc.len() as u32).to_le_bytes());
        pe[section + 20..section + 24].copy_from_slice(&raw.to_le_bytes());
        pe.extend(rsrc);
        pe
    }

    #[test]
    fn icons_come_out_of_the_resources() {
        let image = b"ICONIMAGEBYTES";
        let ico = pe_icon(&sample_pe(image), 0).expect("an icon");
        // ICONDIR, one ICONDIRENTRY with size and offset, then the image.
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(&ico[6..14], &[16, 16, 0, 0, 1, 0, 32, 0]);
        assert_eq!(u32_at(&ico, 14), Some(image.len() as u32));
        assert_eq!(u32_at(&ico, 18), Some(22));
        assert_eq!(&ico[22..], image);
        // The group by resource id (a negative index) and a missing one.
        assert_eq!(pe_icon(&sample_pe(image), -1), Some(ico));
        assert_eq!(pe_icon(&sample_pe(image), -5), None);
        assert_eq!(pe_icon(b"MZ", 0), None);
    }

    #[test]
    fn windows_paths_map_into_the_prefix() {
        let prefix = std::env::temp_dir().join(format!("armor-shortcuts-{}", std::process::id()));
        fs::create_dir_all(prefix.join("drive_c/Program Files/App")).unwrap();
        assert_eq!(
            host_path(&prefix, "c:\\program files\\APP\\app.exe"),
            Some(prefix.join("drive_c/Program Files/App/app.exe"))
        );
        assert_eq!(
            windows_path(&prefix, &prefix.join("drive_c/Program Files/App/app.exe")),
            Some("C:\\Program Files\\App\\app.exe".to_string())
        );
        assert_eq!(host_path(&prefix, "D:\\x"), None);
        fs::remove_dir_all(&prefix).unwrap();
    }
}
