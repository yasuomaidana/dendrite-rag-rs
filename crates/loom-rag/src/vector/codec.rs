//! Reading and writing the vector files: atomic, checksummed writes and a bounds-checked reader.

use std::io::Write;
use std::path::Path;

/// The error of a file that does not parse or does not match its checksum.
pub(crate) fn corrupt(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("corrupt {what}"))
}

/// Writes a file's fields, hashing everything but the trailing checksum.
pub(crate) struct HashedWriter<W: Write> {
    out: W,
    hasher: blake3::Hasher,
}

impl<W: Write> HashedWriter<W> {
    pub(crate) fn put(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.out.write_all(buf)?;
        self.hasher.update(buf);
        Ok(())
    }

    pub(crate) fn put_u32(&mut self, v: u32) -> std::io::Result<()> {
        self.put(&v.to_le_bytes())
    }

    /// A string as a u16 length and its bytes.
    pub(crate) fn put_str(&mut self, s: &str) -> std::io::Result<()> {
        self.put(&(s.len() as u16).to_le_bytes())?;
        self.put(s.as_bytes())
    }

    /// Appends the checksum of everything written so far and flushes.
    fn finish(mut self) -> std::io::Result<()> {
        let checksum = self.hasher.finalize();
        self.out.write_all(checksum.as_bytes())?;
        self.out.flush()
    }
}

/// Writes `path` atomically: `write` fills a temporary file, which gets its checksum, is synced and
/// renamed over `path`; the directory is synced after.
pub(crate) fn write_atomically(
    path: &Path,
    write: impl FnOnce(&mut HashedWriter<std::io::BufWriter<&mut std::fs::File>>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        let mut w = HashedWriter {
            out: std::io::BufWriter::new(&mut file),
            hasher: blake3::Hasher::new(),
        };
        write(&mut w)?;
        w.finish()?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Ok(dir_file) = std::fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

/// Reads a file's fields in order; running out of bytes is a corrupt file.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self { rest: bytes, what }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub(crate) fn corrupt(&self) -> std::io::Error {
        corrupt(self.what)
    }

    pub(crate) fn take(&mut self, n: usize) -> std::io::Result<&'a [u8]> {
        if self.rest.len() < n {
            return Err(self.corrupt());
        }
        let (head, rest) = self.rest.split_at(n);
        self.rest = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> std::io::Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| self.corrupt())
    }

    pub(crate) fn u8(&mut self) -> std::io::Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> std::io::Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn f32(&mut self) -> std::io::Result<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    /// A UTF-8 string with a u16 length (`wide`) or a u8 length, as older files have.
    pub(crate) fn string(&mut self, wide: bool) -> std::io::Result<String> {
        let len = if wide {
            self.u16()? as usize
        } else {
            self.u8()? as usize
        };
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| self.corrupt())
    }
}
