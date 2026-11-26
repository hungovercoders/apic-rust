//! Guessing a Content-Type from the first bytes of a body, for a handler
//! that set none.

/// Returns the media type of `data`, judged by its first 512 bytes the
/// way the WHATWG sniffing algorithm does: HTML, XML, the common image,
/// audio, video, font and archive formats, plain text for anything
/// without binary bytes, else `application/octet-stream`. JSON has no
/// signature, so it is `text/plain; charset=utf-8`.
pub fn detect_content_type(data: &[u8]) -> &'static str {
    let data = &data[..data.len().min(512)];
    let first_non_ws = data
        .iter()
        .position(|b| !matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' '))
        .unwrap_or(data.len());
    let trimmed = &data[first_non_ws..];

    const HTML: [&[u8]; 17] = [
        b"<!DOCTYPE HTML",
        b"<HTML",
        b"<HEAD",
        b"<SCRIPT",
        b"<IFRAME",
        b"<H1",
        b"<DIV",
        b"<FONT",
        b"<TABLE",
        b"<A",
        b"<STYLE",
        b"<TITLE",
        b"<B",
        b"<BODY",
        b"<BR",
        b"<P",
        b"<!--",
    ];
    for sig in HTML {
        // The tag, in any case, followed by a space or `>`.
        if trimmed.len() > sig.len()
            && trimmed[..sig.len()].eq_ignore_ascii_case(sig)
            && matches!(trimmed[sig.len()], b' ' | b'>')
        {
            return "text/html; charset=utf-8";
        }
    }
    if trimmed.starts_with(b"<?xml") {
        return "text/xml; charset=utf-8";
    }
    const EXACT: [(&[u8], &str); 22] = [
        (b"%PDF-", "application/pdf"),
        (b"%!PS-Adobe-", "application/postscript"),
        (b"\x00\x00\x01\x00", "image/x-icon"),
        (b"\x00\x00\x02\x00", "image/x-icon"),
        (b"BM", "image/bmp"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"\x89PNG\x0D\x0A\x1A\x0A", "image/png"),
        (b"\xFF\xD8\xFF", "image/jpeg"),
        (b"ID3", "audio/mpeg"),
        (b"OggS\x00", "application/ogg"),
        (b"MThd\x00\x00\x00\x06", "audio/midi"),
        (b"\x1A\x45\xDF\xA3", "video/webm"),
        (b"\x00\x01\x00\x00", "font/ttf"),
        (b"OTTO", "font/otf"),
        (b"ttcf", "font/collection"),
        (b"wOFF", "font/woff"),
        (b"wOF2", "font/woff2"),
        (b"\x1F\x8B\x08", "application/x-gzip"),
        (b"PK\x03\x04", "application/zip"),
        (b"Rar!\x1A\x07\x00", "application/x-rar-compressed"),
        (b"\x00\x61\x73\x6D", "application/wasm"),
    ];
    if data.starts_with(b"\xFE\xFF") && data.len() >= 4 {
        return "text/plain; charset=utf-16be";
    }
    if data.starts_with(b"\xFF\xFE") && data.len() >= 4 {
        return "text/plain; charset=utf-16le";
    }
    if data.starts_with(b"\xEF\xBB\xBF") && data.len() >= 4 {
        return "text/plain; charset=utf-8";
    }
    for (sig, ct) in EXACT {
        if data.starts_with(sig) {
            return ct;
        }
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" {
        match &data[8..12] {
            b"WEBP" if data.len() >= 14 && &data[12..14] == b"VP" => return "image/webp",
            b"AVI " => return "video/avi",
            b"WAVE" => return "audio/wave",
            _ => {}
        }
    }
    if data.len() >= 12 && &data[..4] == b"FORM" && &data[8..12] == b"AIFF" {
        return "audio/aiff";
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "video/mp4";
    }
    let binary = trimmed.iter().any(|&b| {
        b <= 0x08 || b == 0x0B || (0x0E..=0x1A).contains(&b) || (0x1C..=0x1F).contains(&b)
    });
    if !binary {
        return "text/plain; charset=utf-8";
    }
    "application/octet-stream"
}

#[cfg(test)]
mod tests {
    use super::detect_content_type;

    #[test]
    fn sniffs() {
        for (data, want) in [
            (&b"hello"[..], "text/plain; charset=utf-8"),
            (b"{\"ok\":true}", "text/plain; charset=utf-8"),
            (b"", "text/plain; charset=utf-8"),
            (
                b"  <!doctype html><title>x</title>",
                "text/html; charset=utf-8",
            ),
            (b"<html>", "text/html; charset=utf-8"),
            (b"<p>hi</p>", "text/html; charset=utf-8"),
            (b"<pre>", "text/plain; charset=utf-8"),
            (b"<?xml version=\"1.0\"?>", "text/xml; charset=utf-8"),
            (b"%PDF-1.4\x00", "application/pdf"),
            (b"\x89PNG\x0D\x0A\x1A\x0A....", "image/png"),
            (b"GIF89a....", "image/gif"),
            (b"\x00\x01\x02\x03", "application/octet-stream"),
            (b"\xff\xd8\xff\xe0", "image/jpeg"),
        ] {
            assert_eq!(detect_content_type(data), want, "{data:?}");
        }
    }
}
