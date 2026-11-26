use crate::redact::text_prefix;

/// The cut of --body-limit lands on a character boundary, and only the
/// last character is looked at: an invalid byte earlier is the body's own.
#[test]
fn text_prefix_cases() {
    let euro: &[u8] = b"\xe2\x82\xac"; // a three-byte character
    let cat = |parts: &[&[u8]]| parts.concat();
    for (input, want) in [
        (cat(&[b"abc"]), cat(&[b"abc"])),
        (cat(&[b"ab", euro]), cat(&[b"ab", euro])),
        (cat(&[b"ab", &euro[..2]]), cat(&[b"ab"])),
        (cat(&[b"ab", &euro[..1]]), cat(&[b"ab"])),
        (cat(&[b"\xffabcdefgh"]), cat(&[b"\xffabcdefgh"])),
        (cat(&[b"\xffab", &euro[..1]]), cat(&[b"\xffab"])),
        (cat(&[b""]), cat(&[b""])),
        (cat(&[&euro[..2]]), cat(&[b""])),
        (
            cat(&[b"a\xff\xfe\xfd", euro]),
            cat(&[b"a\xff\xfe\xfd", euro]),
        ),
    ] {
        assert_eq!(text_prefix(&input), &want[..], "{input:?}");
    }
}
