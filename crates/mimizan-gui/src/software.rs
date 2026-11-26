//! The Software tag of a TIFF. A DNG says who wrote it there, and both
//! the X-Trans conversion and the Bayer least-squares file are read
//! through this one function.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// TIFF tag 305, the `Software` string, from the first directories.
/// `None` when the file cannot be read, is not a TIFF, or names no
/// such tag.
pub(crate) fn software_of(path: &Path) -> Option<String> {
    let mut f = BufReader::new(File::open(path).ok()?);
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr).ok()?;
    let le = match &hdr[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    if u16_at(&hdr[2..4], le) != 42 {
        return None;
    }
    let mut offset = u64::from(u32_at(&hdr[4..8], le));
    for _ in 0..8 {
        if offset == 0 {
            break;
        }
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut nbuf = [0u8; 2];
        f.read_exact(&mut nbuf).ok()?;
        let count = usize::from(u16_at(&nbuf, le));
        if count > 4096 {
            return None;
        }
        for _ in 0..count {
            let mut ent = [0u8; 12];
            f.read_exact(&mut ent).ok()?;
            let tag = u16_at(&ent[..2], le);
            let kind = u16_at(&ent[2..4], le);
            let n = u32_at(&ent[4..8], le) as usize;
            // 305 is Software, 2 is ASCII.
            if tag == 305 && kind == 2 && (1..512).contains(&n) {
                let bytes = if n <= 4 {
                    ent[8..8 + n].to_vec()
                } else {
                    let at = u64::from(u32_at(&ent[8..12], le));
                    let back = f.stream_position().ok()?;
                    f.seek(SeekFrom::Start(at)).ok()?;
                    let mut buf = vec![0u8; n];
                    f.read_exact(&mut buf).ok()?;
                    f.seek(SeekFrom::Start(back)).ok()?;
                    buf
                };
                let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                return String::from_utf8(bytes[..end].to_vec()).ok();
            }
        }
        let mut next = [0u8; 4];
        f.read_exact(&mut next).ok()?;
        offset = u64::from(u32_at(&next, le));
    }
    None
}

fn u16_at(b: &[u8], le: bool) -> u16 {
    if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    }
}

fn u32_at(b: &[u8], le: bool) -> u32 {
    if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }
}

/// A one-entry TIFF whose Software tag is `text`, the value inline
/// when it fits in four bytes and after the directory otherwise.
#[cfg(test)]
pub(crate) fn tiff_with_software(text: &str) -> Vec<u8> {
    let bytes = format!("{text}\0").into_bytes();
    let inline = bytes.len() <= 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&305u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    if inline {
        let mut value = [0u8; 4];
        value[..bytes.len()].copy_from_slice(&bytes);
        out.extend_from_slice(&value);
    } else {
        // The directory is 2 + 12 bytes, then the next-IFD word.
        let at = 8 + 2 + 12 + 4;
        out.extend_from_slice(&(at as u32).to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    if !inline {
        out.extend_from_slice(&bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_software_tag_is_read_and_a_file_without_one_is_not() {
        let dir = std::env::temp_dir().join(format!("mimizan-software-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let path = dir.join("shot.dng");
        std::fs::write(&path, tiff_with_software("Mimizan Lab test build")).unwrap();
        assert_eq!(software_of(&path).as_deref(), Some("Mimizan Lab test build"));
        std::fs::write(&path, tiff_with_software("ab")).unwrap();
        assert_eq!(software_of(&path).as_deref(), Some("ab"));
        std::fs::write(&path, b"not a tiff").unwrap();
        assert_eq!(software_of(&path), None);
        assert_eq!(software_of(&dir.join("missing.dng")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
