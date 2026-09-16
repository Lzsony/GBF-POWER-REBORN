use std::io::Read;

const DECODE_LIMIT: usize = 64 * 1024 * 1024;

fn bounded(reader: &mut impl Read, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

pub(super) fn validate(path: &str, headers: &http::HeaderMap, body: &[u8]) -> bool {
    validate_limit(path, headers, body, DECODE_LIMIT)
}

fn validate_limit(path: &str, headers: &http::HeaderMap, body: &[u8], limit: usize) -> bool {
    // Multiple encodings are forwarded, but not admitted to this cache.
    let values: Vec<_> = headers.get_all("content-encoding").iter().collect();
    if values.len() > 1 {
        return false;
    }
    let encoding = match values.first() {
        None => "identity".to_owned(),
        Some(value) => match value.to_str() {
            Ok(value) => value.trim().to_ascii_lowercase(),
            Err(_) => return false,
        },
    };
    let decoded = match encoding.as_str() {
        "identity" => {
            return body.len() <= limit && super::valid_decoded_content(path, headers, body)
        }
        // MultiGzDecoder checks every member's CRC/trailer and rejects trailing garbage.
        "gzip" => bounded(&mut flate2::read::MultiGzDecoder::new(body), limit),
        "deflate" => {
            let mut decoder = flate2::bufread::ZlibDecoder::new(body);
            let decoded = bounded(&mut decoder, limit);
            decoded.filter(|_| decoder.total_in() as usize == body.len())
        }
        "br" => decode_brotli(body, limit),
        _ => None,
    };
    decoded.is_some_and(|bytes| super::valid_decoded_content(path, headers, &bytes))
}

fn decode_brotli(body: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut state = brotli::BrotliState::new(
        brotli::HeapAlloc::<u8>::default(),
        brotli::HeapAlloc::<u32>::default(),
        brotli::HeapAlloc::<brotli::HuffmanCode>::default(),
    );
    let mut remaining = body.len();
    let mut offset = 0;
    let mut total = 0;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 65536];
    loop {
        let mut available = buffer.len();
        let mut written = 0;
        let result = brotli::BrotliDecompressStream(
            &mut remaining,
            &mut offset,
            body,
            &mut available,
            &mut written,
            &mut buffer,
            &mut total,
            &mut state,
        );
        if bytes.len() + written > limit {
            return None;
        }
        bytes.extend_from_slice(&buffer[..written]);
        match result {
            brotli::BrotliResult::ResultSuccess => return (remaining == 0).then_some(bytes),
            brotli::BrotliResult::NeedsMoreOutput => {}
            _ => return None,
        }
    }
}

pub(super) fn valid_policy_body(policy: &str, bytes: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Metadata {
        uri: String,
        #[serde(with = "http_serde::header_map")]
        res: http::HeaderMap,
    }
    let Ok(meta) = serde_json::from_str::<Metadata>(policy) else {
        return false;
    };
    let Ok(uri) = url::Url::parse(&meta.uri) else {
        return false;
    };
    validate(uri.path(), &meta.res, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn encoded(kind: &str, bytes: &[u8]) -> Vec<u8> {
        match kind {
            "gzip" => {
                let mut w = flate2::write::GzEncoder::new(Vec::new(), Default::default());
                w.write_all(bytes).unwrap();
                w.finish().unwrap()
            }
            "deflate" => {
                let mut w = flate2::write::ZlibEncoder::new(Vec::new(), Default::default());
                w.write_all(bytes).unwrap();
                w.finish().unwrap()
            }
            _ => {
                let mut w = brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22);
                w.write_all(bytes).unwrap();
                w.into_inner()
            }
        }
    }
    #[test]
    fn encoded_assets_validate_contents_stream_end_and_budget() {
        for kind in ["gzip", "deflate", "br"] {
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", "application/javascript".parse().unwrap());
            headers.insert("content-encoding", kind.parse().unwrap());
            let bytes = encoded(kind, b"const value = 1;");
            assert!(validate("/assets/a.js", &headers, &bytes), "{kind}");
            assert!(
                !validate("/assets/a.js", &headers, &bytes[..bytes.len() - 1]),
                "truncated {kind}"
            );
            let mut trailing = bytes.clone();
            trailing.extend_from_slice(b"garbage");
            assert!(
                !validate("/assets/a.js", &headers, &trailing),
                "trailing {kind}"
            );
            assert!(!validate(
                "/assets/a.js",
                &headers,
                &encoded(kind, b"<!doctype html><html>error</html>")
            ));
            assert!(!validate(
                "/assets/a.js",
                &headers,
                &encoded(kind, b"\xef\xbb\xbf <!doctype html><html>error</html>")
            ));
            assert!(!validate_limit(
                "/assets/a.js",
                &headers,
                &encoded(kind, &[b'x'; 1025]),
                1024
            ));
            assert!(!validate("/assets/a.js", &headers, b"plain text"));
        }
    }
    #[test]
    fn gzip_members_and_unsupported_chains() {
        let mut headers = http::HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let mut bytes = encoded("gzip", b"const ");
        bytes.extend(encoded("gzip", b"a = 1;"));
        assert!(validate("/assets/a.js", &headers, &bytes));
        bytes[12] ^= 0xff;
        assert!(!validate("/assets/a.js", &headers, &bytes));
        headers.insert("content-encoding", "gzip, br".parse().unwrap());
        assert!(!validate("/assets/a.js", &headers, b"x"));
        headers.insert("content-encoding", "zstd".parse().unwrap());
        assert!(!validate("/assets/a.js", &headers, b"x"));
    }
    #[test]
    fn encoded_invalid_entries_are_removed_on_disk_load() {
        use super::super::*;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        for (index, kind) in ["gzip", "deflate", "br"].iter().enumerate() {
            let uri =
                format!("https://prd-game-a-granbluefantasy.akamaized.net/assets/bad{index}.js");
            let key = CacheKey::from_url(&uri).unwrap();
            let req = Request::builder().uri(&uri).body(()).unwrap();
            let res = Response::builder()
                .header("cache-control", "public,max-age=600")
                .header("content-type", "application/javascript")
                .header("content-encoding", *kind)
                .body(())
                .unwrap();
            let bytes = encoded(kind, b"\xef\xbb\xbf<!doctype html><html>error</html>");
            // A valid checksum does not make an invalid encoded response cacheable.
            cache
                .put(
                    &key,
                    Cached {
                        policy: policy(&req, &res).unwrap(),
                        body: Bytes::from(bytes),
                    },
                    1024 * 1024,
                )
                .unwrap();
        }
        cache.evict_ram();
        for index in 0..3 {
            let uri =
                format!("https://prd-game-a-granbluefantasy.akamaized.net/assets/bad{index}.js");
            let key = CacheKey::from_url(&uri).unwrap();
            assert!(cache.get_disk(&key).unwrap().is_none());
        }
        assert_eq!(cache.usage().unwrap(), 0);
    }
}
