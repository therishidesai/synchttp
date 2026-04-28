use crate::types::{ParseError, ServerConfig};

pub(crate) fn parse_chunked_body(
    bytes: &[u8],
    config: &ServerConfig,
) -> Result<Option<(Vec<u8>, usize)>, ParseError> {
    let mut decoded = Vec::new();
    let mut offset = 0;

    loop {
        let line_end = match find_crlf(bytes, offset) {
            Some(end) => end,
            None => return Ok(None),
        };

        let line = &bytes[offset..line_end];
        let size_bytes = match line.iter().position(|byte| *byte == b';') {
            Some(index) => &line[..index],
            None => line,
        };

        if size_bytes.is_empty() {
            return Err(ParseError::BadRequest("missing chunk size"));
        }

        let size = parse_chunk_size(size_bytes)?;
        offset = line_end + 2;

        if size == 0 {
            if bytes.len() < offset + 2 {
                return Ok(None);
            }

            if &bytes[offset..offset + 2] == b"\r\n" {
                return Ok(Some((decoded, offset + 2)));
            }

            let trailers_end = match find_double_crlf(bytes, offset) {
                Some(end) => end,
                None => return Ok(None),
            };

            if trailers_end != offset {
                return Err(ParseError::NotImplemented(
                    "chunked trailers are not supported",
                ));
            }

            return Ok(Some((decoded, trailers_end + 4)));
        }

        let next_offset = offset
            .checked_add(size)
            .and_then(|value| value.checked_add(2))
            .ok_or(ParseError::PayloadTooLarge)?;

        if decoded.len().saturating_add(size) > config.max_body_bytes {
            return Err(ParseError::PayloadTooLarge);
        }

        if bytes.len() < next_offset {
            return Ok(None);
        }

        decoded.extend_from_slice(&bytes[offset..offset + size]);

        if &bytes[offset + size..next_offset] != b"\r\n" {
            return Err(ParseError::BadRequest("chunk data must end with CRLF"));
        }

        offset = next_offset;
    }
}

fn parse_chunk_size(bytes: &[u8]) -> Result<usize, ParseError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ParseError::BadRequest("invalid chunk size"))?;
    usize::from_str_radix(text, 16).map_err(|_| ParseError::BadRequest("invalid chunk size"))
}

fn find_crlf(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|position| start + position)
}

fn find_double_crlf(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| start + position)
}
