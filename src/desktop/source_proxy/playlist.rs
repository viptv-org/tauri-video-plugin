//! HLS playlist rewriting: every URI an engine would fetch is resolved
//! against the playlist's final URL and pointed back through the proxy.

use url::Url;

/// Largest playlist the proxy buffers and rewrites.
pub const MAX_PLAYLIST_BYTES: usize = 4 * 1024 * 1024;

/// How the proxy serves a rewritten URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    /// A master or media playlist (rewritten again when fetched).
    Playlist,
    /// A media segment, partial segment or init section (sniffed and cleaned).
    Segment { fmp4: bool },
    /// Key material and opaque data: passed through byte-for-byte.
    Raw,
}

/// Whether `body` is an HLS playlist (`#EXTM3U`, allowing a BOM and leading
/// whitespace), independent of its name or `Content-Type`.
pub fn is_playlist(body: &[u8]) -> bool {
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let start = body
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(body.len());
    body[start..].starts_with(b"#EXTM3U")
}

/// Tags whose `URI` attribute names a resource, and how it is served.
fn attribute_resource(tag: &str, fmp4: bool) -> Option<Resource> {
    Some(match tag {
        "#EXT-X-KEY" | "#EXT-X-SESSION-KEY" | "#EXT-X-SESSION-DATA" => Resource::Raw,
        "#EXT-X-MAP" => Resource::Segment { fmp4: true },
        "#EXT-X-PART" | "#EXT-X-PRELOAD-HINT" => Resource::Segment { fmp4 },
        "#EXT-X-MEDIA" | "#EXT-X-I-FRAME-STREAM-INF" | "#EXT-X-RENDITION-REPORT" => {
            Resource::Playlist
        }
        _ => return None,
    })
}

/// Rewrites `text` (fetched from `base`). `route` maps an absolute http(s)
/// URL to its proxy URL; any other scheme (`file:`, `data:`, `skd:` …) is
/// replaced by `blocked`, so a remote playlist can never make an engine read
/// local files or leave the proxy.
pub fn rewrite(
    text: &str,
    base: &Url,
    route: &dyn Fn(Resource, &Url) -> String,
    blocked: &str,
) -> String {
    let resolve = |kind: Resource, reference: &str| -> String {
        match base.join(reference.trim()) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => route(kind, &url),
            _ => blocked.to_owned(),
        }
    };
    let fmp4 = text
        .lines()
        .any(|line| line.trim().starts_with("#EXT-X-MAP"));
    let mut next_uri = None;
    let mut out = String::with_capacity(text.len() * 2);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        if trimmed.starts_with('#') {
            let tag = trimmed.split(':').next().unwrap_or(trimmed);
            if tag == "#EXT-X-STREAM-INF" {
                next_uri = Some(Resource::Playlist);
            }
            match attribute_resource(tag, fmp4) {
                Some(kind) if trimmed.contains(':') => {
                    out.push_str(&rewrite_uri_attribute(trimmed, |uri| resolve(kind, uri)));
                }
                _ => out.push_str(line),
            }
        } else {
            let kind = next_uri.take().unwrap_or(Resource::Segment { fmp4 });
            out.push_str(&resolve(kind, trimmed));
        }
        out.push('\n');
    }
    out
}

/// Replaces the value of the `URI` attribute in a tag's attribute list,
/// respecting quoted values that contain commas.
fn rewrite_uri_attribute(line: &str, replace: impl Fn(&str) -> String) -> String {
    let (tag, attributes) = line.split_once(':').unwrap_or((line, ""));
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in attributes.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ',' if !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(character),
        }
    }
    parts.push(current);
    let parts: Vec<String> = parts
        .into_iter()
        .map(|part| match part.split_once('=') {
            Some((name, value)) if name.trim() == "URI" => {
                let value = value.trim().trim_matches('"');
                format!("URI=\"{}\"", replace(value))
            }
            _ => part,
        })
        .collect();
    format!("{tag}:{}", parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(kind: Resource, url: &Url) -> String {
        let label = match kind {
            Resource::Playlist => "p",
            Resource::Segment { fmp4: false } => "s",
            Resource::Segment { fmp4: true } => "m",
            Resource::Raw => "k",
        };
        format!("proxy/{label}/{url}")
    }

    #[test]
    fn detects_playlists_by_content() {
        assert!(is_playlist(b"#EXTM3U\n"));
        assert!(is_playlist(b"\xef\xbb\xbf \r\n#EXTM3U\n"));
        assert!(!is_playlist(b"\x89PNG"));
    }

    #[test]
    fn media_playlist_segments_and_keys_are_resolved_and_routed() {
        let base = Url::parse("https://cdn.example/live/chan/index.m3u8?token=abc").unwrap();
        let text = "#EXTM3U\r\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin?k=1\",IV=0x01\n#EXTINF:2,\nseg0.png?sig=x\n#EXTINF:2,\n/abs/seg1\n#EXTINF:2,\nhttps://other.example/seg2.css\n#EXT-X-ENDLIST\n";
        let out = rewrite(text, &base, &route, "blocked");
        assert_eq!(
            out,
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"proxy/k/https://cdn.example/live/chan/key.bin?k=1\",IV=0x01\n#EXTINF:2,\nproxy/s/https://cdn.example/live/chan/seg0.png?sig=x\n#EXTINF:2,\nproxy/s/https://cdn.example/abs/seg1\n#EXTINF:2,\nproxy/s/https://other.example/seg2.css\n#EXT-X-ENDLIST\n"
        );
    }

    #[test]
    fn master_playlist_variants_and_renditions_are_playlists() {
        let base = Url::parse("http://origin.example/master.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en, main\",URI=\"audio/en.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"a\"\nvideo/720.m3u8\n#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1,URI=\"iframe.m3u8\"\n";
        let out = rewrite(text, &base, &route, "blocked");
        assert!(
            out.contains("NAME=\"en, main\",URI=\"proxy/p/http://origin.example/audio/en.m3u8\"")
        );
        assert!(out.contains("\nproxy/p/http://origin.example/video/720.m3u8\n"));
        assert!(out.contains("URI=\"proxy/p/http://origin.example/iframe.m3u8\""));
        assert!(out.contains("CODECS=\"avc1.4d401e,mp4a.40.2\""));
    }

    #[test]
    fn fmp4_playlists_route_init_and_segments_as_mp4() {
        let base = Url::parse("http://origin.example/a/index.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.jpg\"\n#EXTINF:2,\nseg0.jpg\n";
        let out = rewrite(text, &base, &route, "blocked");
        assert!(out.contains("URI=\"proxy/m/http://origin.example/a/init.jpg\""));
        assert!(out.contains("\nproxy/m/http://origin.example/a/seg0.jpg\n"));
    }

    #[test]
    fn local_and_opaque_schemes_never_leave_the_proxy() {
        let base = Url::parse("http://origin.example/index.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"file:///etc/passwd\"\n#EXTINF:2,\nfile:///home/user/secret.ts\n#EXTINF:2,\ndata:video/mp2t;base64,AAAA\n";
        let out = rewrite(text, &base, &route, "blocked");
        assert!(!out.contains("file:"));
        assert!(!out.contains("data:"));
        assert_eq!(out.matches("blocked").count(), 3);
    }
}
