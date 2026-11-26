//! A file written whole, or not at all.
//!
//! Every file the programs write for the user goes through [`write_whole`]:
//! the bytes go to `<name>.<pid>.part` beside the final name, are flushed
//! and synced, and the part is then renamed onto the name. The process id
//! keeps two programs that write one name from filling one part. Until
//! that rename the name holds what it held before, so a full disk, a share
//! that drops or a program that is ended costs the new file and never the
//! old one, and a file under its final name is a finished file. A crash
//! leaves the part under a name no later run reuses.
//!
//! Before its own part is made, [`write_whole`] removes a part of the
//! plain older name `<name>.part` when one lies there: a leftover of an
//! earlier version, or of a crash that used that name. The removal is
//! best effort, and an error from it is ignored.
//!
//! One case cannot make the part: a folder that grants the one path alone
//! (the sandboxed Mac gives leave for the path its save panel returned).
//! There the file is written in place, and removed when the write fails.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What an encoder writes into. `write` takes every byte it is given: the
/// `tiff` crate hands a whole strip to one `write` call and counts what
/// came back as the strip, so a short write would be a short strip.
pub struct Sink(BufWriter<File>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Seek for Sink {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

/// The name a file is written under until it is whole. The process id
/// is in it, so two programs writing one name do not share the part.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.part", std::process::id()));
    PathBuf::from(name)
}

/// `<name>.part`, the part name from before the process id was part of it.
fn plain_part(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// An I/O error that names the file it happened on.
fn named(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// Write `path` through `write`. The file is there, whole, when this
/// returns `Ok`; on an error the name is as it was before the call (in a
/// folder that grants only the one path: gone).
pub fn write_whole<T, E>(path: &Path, write: impl FnOnce(&mut Sink) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    // A link is written through, as a plain write does: the file it
    // points at is replaced, and the link stays a link.
    let target = match path.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    };
    // A leftover of the plain older name. This run's own part carries
    // the process id, and a later run does not reuse that name.
    let _ = std::fs::remove_file(plain_part(&target));
    write_part(&target, &part_path(&target), write)
}

/// Write `target` by filling `part` and renaming it onto the name.
/// `part` is this write's own file. A folder that will not take a
/// second name is written in place.
fn write_part<T, E>(target: &Path, part: &Path, write: impl FnOnce(&mut Sink) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    match File::create(part) {
        Ok(file) => {
            let done = fill(file, part, write).and_then(|value| {
                std::fs::rename(part, target).map_err(|e| E::from(named(target, e)))?;
                Ok(value)
            });
            if done.is_err() {
                let _ = std::fs::remove_file(part);
            }
            done
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => write_in_place(target, write),
        Err(e) => Err(E::from(named(part, e))),
    }
}

/// Without a part file: the name is emptied first, and a failure takes
/// the cut-off file away again.
fn write_in_place<T, E>(path: &Path, write: impl FnOnce(&mut Sink) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    let file = File::create(path).map_err(|e| E::from(named(path, e)))?;
    let done = fill(file, path, write);
    if done.is_err() {
        let _ = std::fs::remove_file(path);
    }
    done
}

/// Everything into `file`, down to the disk. The last bytes of a file sit
/// in the buffer, and some volumes report a failed write only when the
/// file is synced: neither may be dropped unseen.
fn fill<T, E>(file: File, path: &Path, write: impl FnOnce(&mut Sink) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    let mut sink = Sink(BufWriter::new(file));
    let value = write(&mut sink)?;
    let file = sink.0.into_inner().map_err(|e| E::from(named(path, e.into_error())))?;
    file.sync_all().map_err(|e| E::from(named(path, e)))?;
    Ok(value)
}

/// Both paths name one file that is there, whatever the spelling: a
/// relative and an absolute path, a link, another name of the same file.
/// A file that is not there yet is no other file's double.
pub fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// [`write_whole`] for bytes that are already there.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_whole(path, |sink| sink.write_all(bytes).map_err(|e| named(path, e)))
}

/// Move `from` onto `to` when `to` is not there.
///
/// A hard link is tried first. It fails where the name is taken, and until
/// `from` is removed the two names are the one file, so nothing is replaced.
/// A volume that cannot make a link (a FAT or exFAT card, some shares) is
/// checked once more and then renamed. [`io::ErrorKind::AlreadyExists`] is
/// returned as such. On any failure `from` is removed.
pub fn place_new(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(from);
            Err(e)
        }
        Err(_) => match std::fs::symlink_metadata(to) {
            Ok(_) => {
                let _ = std::fs::remove_file(from);
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is there already", to.display()),
                ))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => match std::fs::rename(from, to) {
                Ok(()) => Ok(()),
                Err(err) => {
                    let _ = std::fs::remove_file(from);
                    Err(err)
                }
            },
            Err(err) => {
                let _ = std::fs::remove_file(from);
                Err(err)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own, removed when the test ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mimizan-outfile-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_part_name_carries_this_process_and_ends_in_part() {
        let path = Path::new("/photos/print.tif");
        let part = part_path(path);
        assert_eq!(part, PathBuf::from(format!("/photos/print.tif.{}.part", std::process::id())));
        assert!(part.as_os_str().to_string_lossy().ends_with(".part"));
    }

    #[test]
    fn a_stale_plain_part_is_gone_after_a_whole_write() {
        let dir = Dir::new("stale-part");
        let path = dir.0.join("print.tif");
        let stale = plain_part(&path);
        std::fs::write(&stale, b"cut off").unwrap();
        write_bytes(&path, b"whole").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"whole");
        assert!(!stale.exists());
        assert_eq!(dir.names(), ["print.tif"]);
    }

    #[test]
    fn two_writes_of_one_name_each_leave_one_whole_file() {
        let dir = Dir::new("two-writes");
        let path = dir.0.join("print.tif");
        let one = dir.0.join("print.tif.one.part");
        let other = dir.0.join("print.tif.other.part");
        let path_one = path.clone();
        let path_other = path.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                write_part(&path_one, &one, |sink| sink.write_all(b"one whole file")).unwrap();
            });
            scope.spawn(|| {
                write_part(&path_other, &other, |sink| sink.write_all(b"the other whole file")).unwrap();
            });
        });
        let got = std::fs::read(&path).unwrap();
        assert!(
            got == b"one whole file" || got == b"the other whole file",
            "a torn file: {}",
            String::from_utf8_lossy(&got)
        );
        assert_eq!(dir.names(), ["print.tif"]);
    }

    #[test]
    fn a_file_is_written_under_its_name_and_no_part_stays() {
        let dir = Dir::new("whole");
        let path = dir.0.join("print.tif");
        write_bytes(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_bytes(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        assert_eq!(dir.names(), ["print.tif"]);
    }

    #[test]
    fn a_write_that_fails_leaves_the_file_that_was_there() {
        let dir = Dir::new("failed");
        let path = dir.0.join("print.tif");
        write_bytes(&path, b"the earlier print").unwrap();
        let failed: io::Result<()> = write_whole(&path, |sink| {
            sink.write_all(b"half of the new")?;
            Err(io::Error::other("the disk is full"))
        });
        assert_eq!(failed.unwrap_err().to_string(), "the disk is full");
        assert_eq!(std::fs::read(&path).unwrap(), b"the earlier print");
        assert_eq!(dir.names(), ["print.tif"]);
    }

    #[test]
    fn a_write_that_fails_on_a_new_name_leaves_nothing() {
        let dir = Dir::new("nothing");
        let failed: io::Result<()> = write_whole(&dir.0.join("print.tif"), |_| Err(io::Error::other("no")));
        assert!(failed.is_err());
        assert!(dir.names().is_empty());
    }

    #[test]
    fn written_in_place_a_failure_takes_the_cut_off_file_away() {
        let dir = Dir::new("in-place");
        let path = dir.0.join("print.tif");
        write_in_place(&path, |sink| sink.write_all(b"whole")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"whole");
        let failed: io::Result<()> = write_in_place(&path, |sink| {
            sink.write_all(b"cut")?;
            Err(io::Error::other("the share is gone"))
        });
        assert!(failed.is_err());
        assert!(dir.names().is_empty());
    }

    #[test]
    fn a_folder_that_is_not_there_is_named_in_the_error() {
        let dir = Dir::new("missing");
        let path = dir.0.join("no-such-folder").join("print.tif");
        let e = write_bytes(&path, b"x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("no-such-folder"), "{e}");
    }

    #[test]
    fn the_sink_seeks_back_as_a_tiff_encoder_does() {
        let dir = Dir::new("seek");
        let path = dir.0.join("seek.bin");
        write_whole(&path, |sink| -> io::Result<()> {
            sink.write_all(b"....tail")?;
            sink.seek(SeekFrom::Start(0))?;
            sink.write_all(b"head")
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"headtail");
    }

    #[test]
    fn one_file_is_known_under_another_spelling() {
        let dir = Dir::new("same");
        let raw = dir.0.join("shot.dng");
        let other = dir.0.join("other.dng");
        write_bytes(&raw, b"raw").unwrap();
        write_bytes(&other, b"raw").unwrap();
        assert!(same_file(&raw, &raw));
        assert!(same_file(&raw, &dir.0.join(".").join("shot.dng")));
        assert!(!same_file(&raw, &other), "the same bytes are not the same file");
        assert!(!same_file(&raw, &dir.0.join("not-there.dng")));
        assert!(!same_file(&dir.0.join("not-there.dng"), &dir.0.join("not-there.dng")));
        #[cfg(unix)]
        {
            let hard = dir.0.join("second-name.dng");
            std::fs::hard_link(&raw, &hard).unwrap();
            assert!(same_file(&raw, &hard));
            let link = dir.0.join("link.dng");
            std::os::unix::fs::symlink(&raw, &link).unwrap();
            assert!(same_file(&link, &raw));
        }
    }

    #[test]
    fn a_new_file_is_moved_onto_a_free_name() {
        let dir = Dir::new("place-new");
        let from = dir.0.join("shot.dng.mimizan-new");
        let to = dir.0.join("shot.dng");
        std::fs::write(&from, b"the dng").unwrap();
        place_new(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"the dng");
        assert!(!from.exists());
        assert_eq!(dir.names(), ["shot.dng"]);
    }

    #[test]
    fn an_existing_file_is_refused_and_left_as_it_was() {
        let dir = Dir::new("place-taken");
        let from = dir.0.join("shot.dng.mimizan-new");
        let to = dir.0.join("shot.dng");
        std::fs::write(&from, b"the new one").unwrap();
        std::fs::write(&to, b"already there").unwrap();
        let err = place_new(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&to).unwrap(), b"already there");
        assert!(!from.exists(), "no temporary file is left");
        assert_eq!(dir.names(), ["shot.dng"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_written_through_and_stays_a_link() {
        let dir = Dir::new("link");
        let real = dir.0.join("real.json");
        let link = dir.0.join("link.json");
        write_bytes(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_bytes(&link, b"new").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&real).unwrap(), b"new");
        assert_eq!(dir.names(), ["link.json", "real.json"]);
    }
}
