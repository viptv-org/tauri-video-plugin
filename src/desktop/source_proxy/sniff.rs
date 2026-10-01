//! Content classification for HLS media segments.
//!
//! IPTV CDNs disguise segments as images, stylesheets or fonts: the file is
//! named `.png`/`.jpg`/`.css`/…, served with a lying `Content-Type`, and often
//! carries a real image header (or other junk) prepended to the transport
//! stream. Engines typefind those bytes as an image and fail. A segment is
//! therefore classified by its bytes only, never by its name or header, and
//! any leading junk before the first real media structure is dropped.

/// Bytes of a segment head inspected before the body is streamed through.
pub const SNIFF_LIMIT: usize = 64 * 1024;

const TS_PACKET: usize = 188;
const TS_SYNC: u8 = 0x47;
/// Consecutive packets required at the 188-byte stride; a head that ends the
/// run early (a short final segment) still needs [`TS_MIN_RUN`].
const TS_RUN: usize = 5;
const TS_MIN_RUN: usize = 3;
const ADTS_RUN: usize = 3;
const MEDIA_BOXES: [&[u8; 4]; 7] = [
    b"ftyp", b"styp", b"moof", b"moov", b"sidx", b"emsg", b"prft",
];

/// What a segment head turned out to contain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    MpegTs,
    Fmp4,
    /// ADTS packed audio, optionally behind an ID3 timestamp tag.
    Aac,
    /// An ID3-tagged packed-audio segment whose payload is not ADTS.
    PackedAudio,
}

impl SegmentKind {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::MpegTs => "video/mp2t",
            Self::Fmp4 => "video/mp4",
            Self::Aac => "audio/aac",
            Self::PackedAudio => "application/octet-stream",
        }
    }
}

/// The verdict for one segment head: the media kind (when recognised) and the
/// number of leading junk bytes to drop. An unrecognised head is passed
/// through untouched (`offset == 0`, `kind == None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sniffed {
    pub offset: usize,
    pub kind: Option<SegmentKind>,
}

/// Classifies a segment head of at most [`SNIFF_LIMIT`] bytes. `complete`
/// says the head is the whole body, which lets a short final segment match
/// with fewer packets.
pub fn sniff(head: &[u8], complete: bool) -> Sniffed {
    if let Some(kind) = recognized_start(head, complete) {
        return Sniffed {
            offset: 0,
            kind: Some(kind),
        };
    }
    let found = transport_stream_offset(head, complete)
        .map(|at| (at, SegmentKind::MpegTs))
        .or_else(|| fmp4_offset(head).map(|at| (at, SegmentKind::Fmp4)))
        .or_else(|| packed_audio_offset(head, complete));
    match found {
        Some((offset, kind)) => Sniffed {
            offset,
            kind: Some(kind),
        },
        None => Sniffed {
            offset: 0,
            kind: None,
        },
    }
}

/// A head that already starts with a real segment format stays untouched.
fn recognized_start(head: &[u8], complete: bool) -> Option<SegmentKind> {
    // 0x47 alone is also the 'G' of a GIF disguise: a TS start must repeat
    // at the packet stride.
    if ts_run_at(head, 0, complete) {
        return Some(SegmentKind::MpegTs);
    }
    if box_chain_at(head, 0) {
        return Some(SegmentKind::Fmp4);
    }
    if let Some(after) = id3_end(head, 0) {
        return Some(if adts_run_at(head, after, complete) {
            SegmentKind::Aac
        } else {
            SegmentKind::PackedAudio
        });
    }
    adts_run_at(head, 0, complete).then_some(SegmentKind::Aac)
}

fn ts_run_at(head: &[u8], at: usize, complete: bool) -> bool {
    let packets = (0..TS_RUN)
        .map(|index| at + index * TS_PACKET)
        .take_while(|&position| head.get(position) == Some(&TS_SYNC))
        .count();
    packets == TS_RUN
        || (complete && packets >= TS_MIN_RUN && at + packets * TS_PACKET >= head.len())
}

/// Offset of the first MPEG-TS packet run in `head`.
pub fn transport_stream_offset(head: &[u8], complete: bool) -> Option<usize> {
    let scan = head.len().min(SNIFF_LIMIT);
    (0..scan).find(|&at| head[at] == TS_SYNC && ts_run_at(head, at, complete))
}

fn is_media_box(kind: &[u8]) -> bool {
    MEDIA_BOXES.iter().any(|known| known.as_slice() == kind)
}

/// A known top-level box at `at` whose size is plausible and whose successor
/// (when it lies inside the head) is also a box with a printable type.
fn box_chain_at(head: &[u8], at: usize) -> bool {
    let Some(header) = head.get(at..at + 8) else {
        return false;
    };
    if !is_media_box(&header[4..8]) {
        return false;
    }
    let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    // size 1 is a 64-bit largesize box; 0 runs to the end of the file.
    if size == 0 || size == 1 {
        return true;
    }
    if size < 8 {
        return false;
    }
    match head.get(at + size + 4..at + size + 8) {
        Some(next) => next.iter().all(|byte| byte.is_ascii_graphic()),
        None => true,
    }
}

/// Offset of the first fragmented-MP4 box chain in `head`.
pub fn fmp4_offset(head: &[u8]) -> Option<usize> {
    let scan = head.len().min(SNIFF_LIMIT).saturating_sub(8);
    (0..=scan).find(|&at| box_chain_at(head, at))
}

/// End of an ID3v2 tag that starts at `at`.
fn id3_end(head: &[u8], at: usize) -> Option<usize> {
    let tag = head.get(at..at + 10)?;
    let syncsafe = tag[6..10].iter().all(|byte| byte & 0x80 == 0);
    if &tag[..3] != b"ID3" || tag[3] == 0xff || tag[3] > 4 || tag[4] == 0xff || !syncsafe {
        return None;
    }
    let size = tag[6..10]
        .iter()
        .fold(0_usize, |size, byte| (size << 7) | usize::from(*byte));
    let footer = if tag[5] & 0x10 != 0 { 10 } else { 0 };
    Some(at + 10 + size + footer)
}

/// Length of a valid ADTS frame header at `at`.
fn adts_frame(head: &[u8], at: usize) -> Option<usize> {
    let header = head.get(at..at + 7)?;
    let sync = header[0] == 0xff && header[1] & 0xf6 == 0xf0;
    let sampling = (header[2] >> 2) & 0x0f;
    let length = (usize::from(header[3] & 0x03) << 11)
        | (usize::from(header[4]) << 3)
        | usize::from(header[5] >> 5);
    (sync && sampling < 13 && length >= 7).then_some(length)
}

fn adts_run_at(head: &[u8], at: usize, complete: bool) -> bool {
    let mut position = at;
    for frame in 0..ADTS_RUN {
        match adts_frame(head, position) {
            Some(length) => position += length,
            None => return complete && frame > 0 && position >= head.len(),
        }
    }
    true
}

/// Offset of packed audio: an ID3 timestamp tag, or a run of ADTS frames.
fn packed_audio_offset(head: &[u8], complete: bool) -> Option<(usize, SegmentKind)> {
    let scan = head.len().min(SNIFF_LIMIT);
    (0..scan).find_map(|at| {
        if let Some(after) = id3_end(head, at) {
            // A tag that claims more than the window is noise inside junk.
            if after <= head.len() {
                let kind = if adts_run_at(head, after, complete) {
                    SegmentKind::Aac
                } else {
                    SegmentKind::PackedAudio
                };
                return Some((at, kind));
            }
        }
        adts_run_at(head, at, complete).then_some((at, SegmentKind::Aac))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(packets: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for index in 0..packets {
            let mut packet = vec![0xff_u8; TS_PACKET];
            packet[0] = TS_SYNC;
            packet[1] = 0x40;
            packet[3] = 0x10 | (index as u8 & 0x0f);
            out.extend(packet);
        }
        out
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\nIDATx\x9cc\0\x01\0\0\x05\0\x01\r\n-\xb4\0\0\0\0IEND\xaeB`\x82";
    const JPEG: &[u8] = b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0\xff\xdb\0C\0\x08\x06\x06\x07\x06\x05\x08\x07\x07\x07\t\t\x08\n\x0c\x14\r\x0c\x0b\x0b\x0c\x19\x12\x13\x0f\xff\xd9";
    const GIF: &[u8] = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
    const CSS: &[u8] = b"body{background:#000;font-family:Georgia}\n.G{color:#fff}\n";

    #[test]
    fn clean_transport_stream_is_untouched() {
        let body = ts(8);
        assert_eq!(
            sniff(&body, true),
            Sniffed {
                offset: 0,
                kind: Some(SegmentKind::MpegTs)
            }
        );
    }

    #[test]
    fn image_and_text_prefixes_are_stripped_before_the_transport_stream() {
        for prefix in [PNG, JPEG, GIF, CSS] {
            let mut body = prefix.to_vec();
            body.extend(ts(8));
            assert_eq!(
                sniff(&body, true),
                Sniffed {
                    offset: prefix.len(),
                    kind: Some(SegmentKind::MpegTs)
                },
                "prefix {:?}",
                &prefix[..4]
            );
        }
    }

    #[test]
    fn a_gif_g_is_not_mistaken_for_a_sync_byte() {
        // 'G' (0x47) opens the GIF; only the real packets repeat at 188.
        let mut body = GIF.to_vec();
        body.extend(ts(6));
        assert_eq!(sniff(&body, true).offset, GIF.len());
        assert!(!ts_run_at(GIF, 0, true));
    }

    #[test]
    fn short_final_segments_match_with_three_packets_only_when_complete() {
        let mut body = PNG.to_vec();
        body.extend(ts(3));
        assert_eq!(sniff(&body, true).offset, PNG.len());
        assert_eq!(sniff(&body, false).kind, None);
    }

    #[test]
    fn fragmented_mp4_after_junk_is_found() {
        let mut fmp4 = Vec::new();
        fmp4.extend(16_u32.to_be_bytes());
        fmp4.extend(b"styp");
        fmp4.extend(b"msdhmsdh");
        fmp4.extend(24_u32.to_be_bytes());
        fmp4.extend(b"moof");
        fmp4.extend([0; 16]);
        assert_eq!(sniff(&fmp4, true).kind, Some(SegmentKind::Fmp4));
        let mut body = JPEG.to_vec();
        body.extend(&fmp4);
        assert_eq!(
            sniff(&body, true),
            Sniffed {
                offset: JPEG.len(),
                kind: Some(SegmentKind::Fmp4)
            }
        );
    }

    fn adts(frames: usize) -> Vec<u8> {
        let length = 32_usize;
        let mut out = Vec::new();
        for _ in 0..frames {
            let mut frame = vec![0_u8; length];
            frame[0] = 0xff;
            frame[1] = 0xf1;
            frame[2] = 0x50; // AAC LC, 44.1 kHz
            frame[3] = 0x80 | ((length >> 11) as u8 & 0x03);
            frame[4] = (length >> 3) as u8;
            frame[5] = ((length & 0x07) as u8) << 5 | 0x1f;
            frame[6] = 0xfc;
            out.extend(frame);
        }
        out
    }

    #[test]
    fn packed_audio_is_found_after_junk_and_behind_id3() {
        let mut body = PNG.to_vec();
        body.extend(adts(4));
        assert_eq!(
            sniff(&body, true),
            Sniffed {
                offset: PNG.len(),
                kind: Some(SegmentKind::Aac)
            }
        );
        let mut tagged = b"ID3\x04\0\0\0\0\0\x05PRIV\0".to_vec();
        tagged.extend(adts(4));
        assert_eq!(sniff(&tagged, true).kind, Some(SegmentKind::Aac));
        let mut disguised = CSS.to_vec();
        disguised.extend(&tagged);
        assert_eq!(sniff(&disguised, true).offset, CSS.len());
    }

    #[test]
    fn unknown_bodies_pass_through() {
        assert_eq!(
            sniff(PNG, true),
            Sniffed {
                offset: 0,
                kind: None
            }
        );
        assert_eq!(sniff(b"", true).kind, None);
    }
}
