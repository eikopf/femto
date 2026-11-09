use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom};

/// KOReader's sampled content hash, distinct from Femto's local book ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DocumentId(String);
impl DocumentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reads at most twelve 1 KiB samples and restores the caller's file position.
    pub fn sampled(source: &mut (impl Read + Seek)) -> io::Result<Self> {
        let position = source.stream_position()?;
        let result = (|| {
            let mut hash = Md5::new();
            let mut buffer = [0; 1024];
            for offset in std::iter::once(0).chain((0..=10).map(|i| 1024u64 << (i * 2))) {
                source.seek(SeekFrom::Start(offset))?;
                let mut filled = 0;
                while filled < buffer.len() {
                    match source.read(&mut buffer[filled..]) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    }
                }
                if filled == 0 {
                    break;
                }
                hash.update(&buffer[..filled]);
            }
            Ok(Self(format!("{:x}", hash.finalize())))
        })();
        source.seek(SeekFrom::Start(position))?;
        result
    }

    /// The UTF-8 basename includes its extension and is not case-normalized.
    pub fn filename(basename: &str) -> Result<Self, crate::Error> {
        if basename.is_empty() || basename.contains(['/', '\0']) {
            return Err(crate::Error::Invalid("filename"));
        }
        Ok(Self(format!("{:x}", Md5::digest(basename.as_bytes()))))
    }
}
impl TryFrom<String> for DocumentId {
    type Error = crate::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(crate::Error::Invalid("document checksum"));
        }
        Ok(Self(value))
    }
}
impl From<DocumentId> for String {
    fn from(id: DocumentId) -> Self {
        id.0
    }
}
