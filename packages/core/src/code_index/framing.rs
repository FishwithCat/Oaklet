use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// Bound the frame before async-lsp allocates its Content-Length-sized buffer.
pub async fn forward_bounded(
    input: impl AsyncRead + Unpin,
    mut output: impl tokio::io::AsyncWrite + Unpin,
) -> std::io::Result<()> {
    let mut input = BufReader::new(input);
    loop {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            if header.len() == MAX_HEADER_BYTES {
                return Err(std::io::Error::other("LSP header exceeds 8 KiB"));
            }
            match input.read_u8().await {
                Ok(byte) => header.push(byte),
                Err(error)
                    if header.is_empty() && error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        }
        let header_text = std::str::from_utf8(&header).map_err(std::io::Error::other)?;
        let mut length = None;
        for line in header_text.split("\r\n").filter(|line| !line.is_empty()) {
            let (name, value) = line
                .split_once(": ")
                .ok_or_else(|| std::io::Error::other("Invalid LSP header"))?;
            if name.eq_ignore_ascii_case("Content-Length") {
                if length.is_some() {
                    return Err(std::io::Error::other("Duplicate Content-Length"));
                }
                length = Some(value.parse::<usize>().map_err(std::io::Error::other)?);
            }
        }
        let length = length.ok_or_else(|| std::io::Error::other("Missing Content-Length"))?;
        if length > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::other("LSP message exceeds 8 MiB"));
        }
        output.write_all(&header).await?;
        let copied = tokio::io::copy(&mut (&mut input).take(length as u64), &mut output).await?;
        if copied != length as u64 {
            return Err(std::io::Error::other("Truncated LSP body"));
        }
        output.flush().await?;
    }
}
