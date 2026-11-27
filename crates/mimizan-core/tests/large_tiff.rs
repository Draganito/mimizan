//! A picture too large for a classic TIFF, whose offsets are 32 bits.
//! Not part of the usual run: it writes 4.4 GB under the target
//! directory. By hand:
//!
//! `cargo test --release -p mimizan-core --test large_tiff -- --ignored`

use mimizan_core::tiffin::read_gray_header;
use mimizan_core::tiffout::write_gray8;
use tiff::decoder::{Decoder, DecodingResult, Limits};

const DESCRIPTION: &str = r#"{"stage":"test"}"#;

#[test]
#[ignore = "writes 4.4 GB under target/"]
fn a_picture_past_four_gigabytes_is_written_whole() {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("large.tif");
    let (w, h) = (66_000usize, 66_000usize);
    assert!((w * h) as u64 > u64::from(u32::MAX));
    // Zeros but for three samples: the pages in between are never
    // touched, so the picture takes the room of the file and no memory.
    let mut data = vec![0u8; w * h];
    data[0] = 7;
    data[w * h / 2 + 5] = 8;
    data[w * h - 1] = 9;
    let written = write_gray8(&path, w, h, &data, DESCRIPTION);
    drop(data);
    let read = written.map_err(|e| e.to_string()).and_then(|()| {
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        let description = read_gray_header(&path).map_err(|e| e.to_string())?;
        let file = std::io::BufReader::new(std::fs::File::open(&path).map_err(|e| e.to_string())?);
        let mut dec = Decoder::new(file).map_err(|e| e.to_string())?.with_limits(Limits::unlimited());
        let dims = dec.dimensions().map_err(|e| e.to_string())?;
        let strips = dec.strip_count().map_err(|e| e.to_string())?;
        let mut sample = |strip: u32, at: usize| -> Result<u8, String> {
            match dec.read_chunk(strip).map_err(|e| e.to_string())? {
                DecodingResult::U8(v) => Ok(v[at]),
                _ => Err("not 8 bit".into()),
            }
        };
        // 256 rows a strip: the first sample, the one past the middle,
        // the last.
        let middle = h / 2 / 256;
        let last_rows = h - (strips as usize - 1) * 256;
        let samples = [
            sample(0, 0)?,
            sample(middle as u32, (h / 2 - middle * 256) * w + 5)?,
            sample(strips - 1, last_rows * w - 1)?,
        ];
        Ok((size, description, dims, samples))
    });
    let _ = std::fs::remove_file(&path);
    let (size, description, dims, samples) = read.unwrap();
    assert!(size > (w * h) as u64, "{size} bytes");
    assert_eq!(description.as_deref(), Some(DESCRIPTION));
    assert_eq!(dims, (w as u32, h as u32));
    assert_eq!(samples, [7, 8, 9]);
}
