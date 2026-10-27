//! Size-capped HTTP response body reads.

use anyhow::Result;

/// Read a whole response body, failing as soon as it would exceed `max_bytes`.
///
/// A declared `Content-Length` over the cap is refused before anything is
/// read; chunked or length-less bodies are bounded while streaming, so a
/// server cannot make the caller buffer an unbounded body before a size check.
pub async fn read_response_body_capped(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    use futures_util::StreamExt;

    if let Some(len) = response.content_length()
        && len > max_bytes as u64
    {
        anyhow::bail!("response body of {len} bytes exceeds {max_bytes} bytes — aborting");
    }
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("failed to read response body: {e}"))?;
        if buf.len().saturating_add(chunk.len()) > max_bytes {
            anyhow::bail!("response body exceeds {max_bytes} bytes — aborting");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}
