//! Minimal ICC v4 grey and RGB profiles generated in code (no colour
//! management library).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GrayTrc {
    /// kTRC identity: linear light.
    Linear,
    /// kTRC gamma, u8Fixed8 (2.2 -> 0x0233).
    Gamma(f64),
    /// The curve of IEC 61966-2-1 (sRGB): a straight toe below 0.04045,
    /// the 2.4 power with its offset above. Written as a parametric curve.
    Srgb,
}

/// The description of the profile with sRGB's primaries and sRGB's curve:
/// the standard's own name, which is what a print service looks for.
pub const SRGB_NAME: &str = "sRGB IEC61966-2.1";

/// The description of the profile with the primaries and the gamma of
/// Adobe RGB (1998). Adobe keeps the bare name for the profile it ships
/// itself and asks for this wording on any other.
pub const ADOBE_COMPATIBLE_NAME: &str = "compatible with Adobe RGB (1998)";

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

/// The parameters of the sRGB curve as the parametric type 3 takes them,
/// `g a b c d`: `Y = (aX + b)^g` from `d` up, `Y = cX` below.
const SRGB_PARA: [f64; 5] = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045];

fn curv_tag(trc: GrayTrc) -> Vec<u8> {
    let mut t = Vec::new();
    let (kind, body): (&[u8; 4], Vec<u8>) = match trc {
        GrayTrc::Linear => (b"curv", be32(0).to_vec()),
        GrayTrc::Gamma(g) => {
            (b"curv", [&be32(1)[..], &((g * 256.0).round() as u16).to_be_bytes()[..]].concat())
        }
        // Function type 3, two reserved bytes, the five parameters.
        GrayTrc::Srgb => (b"para", [&[0, 3, 0, 0][..], &SRGB_PARA.map(s15f16).concat()[..]].concat()),
    };
    t.extend_from_slice(kind);
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&body);
    pad4(&mut t);
    t
}

/// The description of the grey profile of `trc`.
pub fn gray_profile_name(trc: GrayTrc) -> &'static str {
    match trc {
        GrayTrc::Linear => "mimizan linear gray (gamma 1.0)",
        GrayTrc::Gamma(_) => "mimizan gray gamma 2.2",
        GrayTrc::Srgb => "mimizan gray, sRGB curve",
    }
}

/// Build a GRAY/XYZ display-class profile with the given tone curve.
pub fn gray_profile(trc: GrayTrc) -> Vec<u8> {
    let name = gray_profile_name(trc);
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", mluc(name)),
        (b"cprt", mluc("No copyright, use freely")),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"kTRC", curv_tag(trc)),
    ];
    assemble(b"GRAY", tags)
}

/// The description of the RGB profile of `space` and `trc`. The two a
/// print service knows carry the names it knows them by; the others say
/// what they are.
pub fn rgb_profile_name(space: crate::plate::PlateSpace, trc: GrayTrc) -> &'static str {
    use crate::plate::PlateSpace::{AdobeRgb, Srgb};
    match (space, trc) {
        (Srgb, GrayTrc::Srgb) => SRGB_NAME,
        (AdobeRgb, GrayTrc::Gamma(_)) => ADOBE_COMPATIBLE_NAME,
        (AdobeRgb, GrayTrc::Linear) => "mimizan plate Adobe RGB (1998) primaries, linear",
        (AdobeRgb, GrayTrc::Srgb) => "mimizan plate Adobe RGB (1998) primaries, sRGB curve",
        (Srgb, GrayTrc::Gamma(_)) => "mimizan plate sRGB primaries gamma 2.2",
        (Srgb, GrayTrc::Linear) => "mimizan plate sRGB primaries, linear",
    }
}

/// Build an RGB/XYZ matrix-TRC display-class profile: the colorants of
/// `space` (D50-adapted) and one `trc` for all three channels. The plate's
/// print file carries this, and so does a grey sheet written as RGB.
pub fn rgb_profile(space: crate::plate::PlateSpace, trc: GrayTrc) -> Vec<u8> {
    let name = rgb_profile_name(space, trc);
    let [r, g, b] = space.colorants_d50();
    let curve = curv_tag(trc);
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", mluc(name)),
        (b"cprt", mluc("No copyright, use freely")),
        (b"wtpt", xyz_tag(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz_tag(r[0], r[1], r[2])),
        (b"gXYZ", xyz_tag(g[0], g[1], g[2])),
        (b"bXYZ", xyz_tag(b[0], b[1], b[2])),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    assemble(b"RGB ", tags)
}

/// Header, tag table and tag data of a display-class profile with PCS XYZ.
fn assemble(colour_space: &[u8; 4], tags: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut header = vec![0u8; 128];
    header[8..12].copy_from_slice(&be32(0x0420_0000));
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(colour_space);
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
        let rgb = rgb_profile(crate::plate::PlateSpace::AdobeRgb, GrayTrc::Gamma(2.2));
        assert_eq!(&rgb[36..40], b"acsp");
        assert_eq!(&rgb[16..20], b"RGB ");
        assert_eq!(u32::from_be_bytes([rgb[0], rgb[1], rgb[2], rgb[3]]) as usize, rgb.len());
        assert_eq!(u32::from_be_bytes([rgb[128], rgb[129], rgb[130], rgb[131]]), 9);
        // The colorants add up to the D50 white.
        for s in crate::plate::PlateSpace::ALL {
            let c = s.colorants_d50();
            for k in 0..3 {
                let sum: f64 = c.iter().map(|v| v[k]).sum();
                let want = [0.9642, 1.0, 0.8249][k];
                assert!((sum - want).abs() < 1e-3, "{s:?} {k} {sum}");
            }
        }
    }

    /// The tag `sig` of a profile: its bytes, from the tag table.
    fn tag<'a>(profile: &'a [u8], sig: &[u8; 4]) -> &'a [u8] {
        let n = u32::from_be_bytes(profile[128..132].try_into().unwrap()) as usize;
        let word = |at: usize| u32::from_be_bytes(profile[at..at + 4].try_into().unwrap()) as usize;
        let entry = (0..n).map(|i| 132 + 12 * i).find(|&at| &profile[at..at + 4] == sig).expect("the tag");
        &profile[word(entry + 4)..word(entry + 4) + word(entry + 8)]
    }

    /// The text of a `desc` tag written by `mluc`.
    fn description(profile: &[u8]) -> String {
        let t = tag(profile, b"desc");
        assert_eq!(&t[..4], b"mluc");
        let len = u32::from_be_bytes(t[20..24].try_into().unwrap()) as usize;
        let units: Vec<u16> =
            t[28..28 + len].as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16(&units).unwrap()
    }

    /// A parametric curve of type 3 read back and evaluated at `x`.
    fn para(t: &[u8], x: f64) -> f64 {
        assert_eq!(&t[..4], b"para");
        assert_eq!(u16::from_be_bytes([t[8], t[9]]), 3);
        assert_eq!(t.len(), 12 + 5 * 4);
        let v = |i: usize| i32::from_be_bytes(t[12 + 4 * i..16 + 4 * i].try_into().unwrap()) as f64 / 65536.0;
        let (g, a, b, c, d) = (v(0), v(1), v(2), v(3), v(4));
        if x >= d {
            (a * x + b).powf(g)
        } else {
            c * x
        }
    }

    /// The two profiles a print service knows are named as it knows them,
    /// and the sRGB one carries the sRGB curve: the curve in the file
    /// undoes `kodak::srgb_encode`, which is what the print's values are
    /// encoded with.
    #[test]
    fn the_standard_profiles_are_named_and_curved_as_the_standards() {
        use crate::plate::PlateSpace;
        let srgb = rgb_profile(PlateSpace::Srgb, GrayTrc::Srgb);
        assert_eq!(description(&srgb), "sRGB IEC61966-2.1");
        assert_eq!(u32::from_be_bytes(srgb[0..4].try_into().unwrap()) as usize, srgb.len());
        assert_eq!(srgb.len() % 4, 0);
        for channel in [b"rTRC", b"gTRC", b"bTRC"] {
            let curve = tag(&srgb, channel);
            for k in 0..=1000 {
                let linear = k as f64 / 1000.0;
                let back = para(curve, crate::kodak::srgb_encode(linear));
                assert!((back - linear).abs() < 2e-4, "{linear} came back as {back}");
            }
        }
        // Adobe's own name is Adobe's; a profile made here says so.
        let adobe = rgb_profile(PlateSpace::AdobeRgb, GrayTrc::Gamma(2.2));
        assert_eq!(description(&adobe), "compatible with Adobe RGB (1998)");
        // Its curve is the plain gamma 563/256 of that space.
        let curve = tag(&adobe, b"gTRC");
        assert_eq!(&curve[..4], b"curv");
        assert_eq!(curve[8..14], [0, 0, 0, 1, 0x02, 0x33]);
        // The grey sheet keeps its grey profile.
        assert_eq!(description(&gray_profile(GrayTrc::Gamma(2.2))), "mimizan gray gamma 2.2");
    }
}
