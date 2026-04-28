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

#[cfg(test)]
mod tests {
    use super::parse_chunked_body;
    use crate::types::{ParseError, ServerConfig};
    use proptest::prelude::*;

    fn encode_chunked(body: &[u8], split_points: &[usize], with_extensions: bool) -> Vec<u8> {
        let mut normalized = split_points.to_vec();
        normalized.sort_unstable();
        normalized.dedup();

        let mut chunks = Vec::new();
        let mut start = 0usize;

        for point in normalized {
            if point <= start || point >= body.len() {
                continue;
            }
            chunks.push(&body[start..point]);
            start = point;
        }

        if start < body.len() {
            chunks.push(&body[start..]);
        }

        let mut encoded = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let mut line = format!("{:X}", chunk.len());
            if with_extensions && index % 2 == 0 {
                line.push_str(";test=value");
            }
            encoded.extend_from_slice(line.as_bytes());
            encoded.extend_from_slice(b"\r\n");
            encoded.extend_from_slice(chunk);
            encoded.extend_from_slice(b"\r\n");
        }

        encoded.extend_from_slice(b"0\r\n\r\n");
        encoded
    }

    proptest! {
        #[test]
        fn chunked_round_trip_preserves_body(
            body in proptest::collection::vec(any::<u8>(), 0..128),
            split_points in proptest::collection::vec(0usize..128usize, 0..12),
            with_extensions in any::<bool>(),
        ) {
            let encoded = encode_chunked(&body, &split_points, with_extensions);
            let config = ServerConfig::default().max_body_bytes(256);
            let parsed = parse_chunked_body(&encoded, &config).unwrap().unwrap();

            prop_assert_eq!(parsed.0, body);
            prop_assert_eq!(parsed.1, encoded.len());
        }

        #[test]
        fn chunked_round_trip_matches_incremental_delivery(
            body in proptest::collection::vec(any::<u8>(), 0..96),
            chunk_splits in proptest::collection::vec(0usize..96usize, 0..8),
            delivery_splits in proptest::collection::vec(0usize..256usize, 0..10),
            with_extensions in any::<bool>(),
        ) {
            let encoded = encode_chunked(&body, &chunk_splits, with_extensions);
            let config = ServerConfig::default().max_body_bytes(256);
            let direct = parse_chunked_body(&encoded, &config).unwrap().unwrap();

            let mut buffer = Vec::new();
            let mut offset = 0usize;
            let mut incremental = None;
            let mut delivery_splits = delivery_splits;
            delivery_splits.push(encoded.len());
            delivery_splits.sort_unstable();
            delivery_splits.dedup();

            for point in delivery_splits {
                let end = point.min(encoded.len());
                if end <= offset {
                    continue;
                }

                buffer.extend_from_slice(&encoded[offset..end]);
                offset = end;

                if let Some(parsed) = parse_chunked_body(&buffer, &config).unwrap() {
                    incremental = Some(parsed);
                    break;
                }
            }

            if incremental.is_none() && offset < encoded.len() {
                buffer.extend_from_slice(&encoded[offset..]);
                incremental = parse_chunked_body(&buffer, &config).unwrap();
            }

            let incremental = incremental.expect("chunked body should parse once all bytes are present");
            prop_assert_eq!(direct.0, incremental.0);
            prop_assert_eq!(direct.1, incremental.1);
        }

        #[test]
        fn chunked_parser_enforces_body_limit(
            body in proptest::collection::vec(any::<u8>(), 17..64),
            split_points in proptest::collection::vec(0usize..64usize, 0..8),
        ) {
            let encoded = encode_chunked(&body, &split_points, true);
            let config = ServerConfig::default().max_body_bytes(16);
            let error = parse_chunked_body(&encoded, &config).unwrap_err();

            prop_assert_eq!(error, ParseError::PayloadTooLarge);
        }

        #[test]
        fn chunked_parser_never_panics_on_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..256)) {
            let config = ServerConfig::default();
            let _ = parse_chunked_body(&data, &config);
        }
    }
}
