use std::io::{self, Cursor, Read};

use flate2::read::GzDecoder;

const STREAM_CAP_FACTOR: u64 = 4;

#[derive(Debug)]
struct StreamLimitExceeded;

impl std::fmt::Display for StreamLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decompressed archive stream exceeds the size limit")
    }
}

impl std::error::Error for StreamLimitExceeded {}

pub struct BoundedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.remaining = self
            .remaining
            .checked_sub(read as u64)
            .ok_or_else(|| io::Error::other(StreamLimitExceeded))?;
        Ok(read)
    }
}

pub type BoundedTarball<'a> = tar::Archive<BoundedReader<GzDecoder<Cursor<&'a [u8]>>>>;

pub fn stream_cap(max_size: u64) -> u64 {
    max_size.saturating_mul(STREAM_CAP_FACTOR)
}

pub fn open_bounded_tarball(bytes: &[u8], max_size: u64) -> BoundedTarball<'_> {
    tar::Archive::new(BoundedReader {
        inner: GzDecoder::new(Cursor::new(bytes)),
        remaining: stream_cap(max_size),
    })
}

pub fn stream_limit_exceeded(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
            .is_some_and(|inner| inner.is::<StreamLimitExceeded>())
    })
}

pub fn map_stream_limit(
    err: anyhow::Error,
    oversized: impl FnOnce() -> anyhow::Error,
) -> anyhow::Error {
    if stream_limit_exceeded(&err) {
        oversized()
    } else {
        err
    }
}

pub fn read_body_within(reader: &mut impl Read, remaining: u64) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    reader
        .take(remaining.saturating_add(1))
        .read_to_end(&mut buf)?;
    Ok((buf.len() as u64 <= remaining).then_some(buf))
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_archives {
    use std::io::Read;

    type Builder = tar::Builder<flate2::write::GzEncoder<Vec<u8>>>;

    fn builder() -> Builder {
        tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::best(),
        ))
    }

    fn finish(builder: Builder) -> Vec<u8> {
        builder
            .into_inner()
            .expect("tar trailer")
            .finish()
            .expect("gzip trailer")
    }

    fn add(
        builder: &mut Builder,
        kind: tar::EntryType,
        path: &str,
        declared: u64,
        body: impl Read,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_path(path).expect("path");
        header.set_size(declared);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, body).expect("append");
    }

    fn filler(byte: u8, len: u64) -> impl Read {
        std::io::repeat(byte).take(len)
    }

    fn pax_size_record(size: u64) -> String {
        let body = format!(" size={size}\n");
        let mut total = body.len();
        while total != body.len() + total.to_string().len() {
            total = body.len() + total.to_string().len();
        }
        format!("{total}{body}")
    }

    pub fn pax_size_record_archive(first: (&str, &[u8]), body_len: u64) -> Vec<u8> {
        let mut archive = builder();
        add(
            &mut archive,
            tar::EntryType::Regular,
            first.0,
            first.1.len() as u64,
            first.1,
        );
        let record = pax_size_record(body_len);
        add(
            &mut archive,
            tar::EntryType::XHeader,
            "PaxHeader/blob",
            record.len() as u64,
            record.as_bytes(),
        );
        add(
            &mut archive,
            tar::EntryType::Regular,
            "blob",
            0,
            filler(0, body_len),
        );
        finish(archive)
    }

    pub fn extension_body_archive(
        first: (&str, &[u8]),
        kind: tar::EntryType,
        body_len: u64,
    ) -> Vec<u8> {
        let mut archive = builder();
        add(
            &mut archive,
            tar::EntryType::Regular,
            first.0,
            first.1.len() as u64,
            first.1,
        );
        add(&mut archive, kind, "ext", body_len, filler(b'a', body_len));
        add(&mut archive, tar::EntryType::Regular, "blob", 0, &b""[..]);
        finish(archive)
    }

    pub fn empty_entries_archive(first: (&str, &[u8]), count: usize) -> Vec<u8> {
        let mut archive = builder();
        add(
            &mut archive,
            tar::EntryType::Regular,
            first.0,
            first.1.len() as u64,
            first.1,
        );
        for index in 0..count {
            add(
                &mut archive,
                tar::EntryType::Regular,
                &format!("empty-{index}"),
                0,
                &b""[..],
            );
        }
        finish(archive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_read_accepts_exactly_the_remaining_budget() {
        let data = [7u8; 10];
        assert_eq!(
            read_body_within(&mut &data[..], 10).unwrap().unwrap().len(),
            10
        );
        assert!(read_body_within(&mut &data[..], 9).unwrap().is_none());
        assert!(read_body_within(&mut &data[..], 0).unwrap().is_none());
        assert!(read_body_within(&mut &[][..], 0).unwrap().is_some());
    }

    #[test]
    fn reader_errors_instead_of_truncating_past_the_limit() {
        let mut reader = BoundedReader {
            inner: &[1u8; 100][..],
            remaining: 64,
        };
        let mut sink = Vec::new();
        let err = reader.read_to_end(&mut sink).unwrap_err();
        assert!(
            err.get_ref()
                .is_some_and(|inner| inner.is::<StreamLimitExceeded>())
        );
        assert!(stream_limit_exceeded(&anyhow::Error::from(err)));
    }
}
