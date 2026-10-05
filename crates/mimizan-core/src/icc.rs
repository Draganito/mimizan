//! Minimal ICC v4 grey profiles generated in code (no colour management library).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GrayTrc {
    /// kTRC identity: linear light.
    Linear,
    /// kTRC gamma, u8Fixed8 (2.2 -> 0x0233).
    Gamma(f64),
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

fn mluc(text: &str) -> Vec<u8> {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    let mut t = Vec::new();
    t.extend_from_slice(b"mluc");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&be32(1));
    t.extend_from_slice(&be32(12));
    t.extend_from_slice(b"enUS");
    t.extend_from_slice(&be32(utf16.len() as u32));
    t.extend_from_slice(&be32(28));
    t.extend_from_slice(&utf16);
    pad4(&mut t);
    t
}

fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"XYZ ");
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&s15f16(x));
    t.extend_from_slice(&s15f16(y));
    t.extend_from_slice(&s15f16(z));
    t
}

fn curv_tag(trc: GrayTrc) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"curv");
    t.extend_from_slice(&[0; 4]);
    match trc {
        GrayTrc::Linear => t.extend_from_slice(&be32(0)),
        GrayTrc::Gamma(g) => {
            t.extend_from_slice(&be32(1));
            t.extend_from_slice(&((g * 256.0).round() as u16).to_be_bytes());
        }
    }
    pad4(&mut t);
    t
}

/// Build a GRAY/XYZ display-class profile with the given tone curve.
pub fn gray_profile(trc: GrayTrc) -> Vec<u8> {
    let name = match trc {
        GrayTrc::Linear => "mimizan linear gray (gamma 1.0)",
        GrayTrc::Gamma(_) => "mimizan gray gamma 2.2",
    };
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", mluc(name)),
        (b"cprt", mluc("No copyright, use freely")),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"kTRC", curv_tag(trc)),
    ];

    let mut header = vec![0u8; 128];
    header[8..12].copy_from_slice(&be32(0x0420_0000));
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(b"GRAY");
    header[20..24].copy_from_slice(b"XYZ ");
    // date/time left zero (allowed); 'acsp' signature:
    header[36..40].copy_from_slice(b"acsp");
    header[68..72].copy_from_slice(&s15f16(0.9642));
    header[72..76].copy_from_slice(&s15f16(1.0));
    header[76..80].copy_from_slice(&s15f16(0.8249));

    let table_len = 4 + 12 * tags.len();
    let mut body = Vec::new();
    let mut table = Vec::new();
    table.extend_from_slice(&be32(tags.len() as u32));
    let mut offset = 128 + table_len;
    for (sig, data) in &tags {
        table.extend_from_slice(*sig);
        table.extend_from_slice(&be32(offset as u32));
        table.extend_from_slice(&be32(data.len() as u32));
        body.extend_from_slice(data);
        offset += data.len();
    }
    let mut out = header;
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&be32(size));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_well_formed() {
        let p = gray_profile(GrayTrc::Linear);
        assert_eq!(&p[36..40], b"acsp");
        assert_eq!(u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize, p.len());
        assert_eq!(&p[16..20], b"GRAY");
        let g = gray_profile(GrayTrc::Gamma(2.2));
        assert!(g.len() > 128);
    }
}
