use std::io::Write;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::memory_pipe;
use crate::input::{BgSource, KeySource, ReadError};

/// One read's outcome, carried over a channel so a test can wait on it
/// with a timeout.
pub(crate) type ReadResult = Result<String, ReadError>;

pub(crate) fn read_async(src: &Arc<dyn KeySource>) -> mpsc::Receiver<ReadResult> {
    let (tx, rx) = mpsc::channel();
    let src = src.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let out = src
            .read(&mut buf)
            .map(|n| String::from_utf8_lossy(&buf[..n]).into_owned());
        let _ = tx.send(out);
    });
    rx
}

pub(crate) fn wait_read(c: &mpsc::Receiver<ReadResult>) -> ReadResult {
    c.recv_timeout(Duration::from_secs(2))
        .expect("the read did not return")
}

// The fallback source, used where poll is not available: cancel unblocks
// a pending read, resume drops what arrived in between, and the next read
// sees only what came after.
#[test]
fn bg_source_cancel_and_resume() {
    let (pr, mut pw) = memory_pipe();
    let bg = Arc::new(BgSource::new(pr));
    let s: Arc<dyn KeySource> = bg.clone();

    let pending = read_async(&s);
    s.cancel();
    assert_eq!(
        wait_read(&pending),
        Err(ReadError::Cancelled),
        "cancel should end the pending read"
    );
    assert_eq!(
        wait_read(&read_async(&s)),
        Err(ReadError::Cancelled),
        "a read after cancel should fail at once"
    );

    // Typed while paused: taken by the background read, which marks it
    // stale, so it is dropped however late it is consumed. Wait for it to
    // be queued so that resume cannot run before the mark is taken.
    pw.write_all(b"zzz").expect("write");
    let start = Instant::now();
    while bg.queued() == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the reader should have queued the chunk"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    s.resume();
    let after = read_async(&s);
    pw.write_all(b"a").expect("write");
    assert_eq!(
        wait_read(&after),
        Ok("a".to_string()),
        "after resume the first read should see only what was typed after it"
    );

    // A read shorter than the chunk keeps the rest for the next one.
    pw.write_all(b"hello").expect("write");
    let mut one = [0u8; 2];
    assert_eq!(s.read(&mut one), Ok(2), "short read");
    assert_eq!(&one, b"he", "short read");
    let mut rest = [0u8; 8];
    let n = s.read(&mut rest).expect("the rest");
    assert_eq!(&rest[..n], b"llo", "rest");

    // The reader's own error ends every read from then on.
    drop(pw);
    for i in 0..2 {
        assert_eq!(
            wait_read(&read_async(&s)),
            Err(ReadError::eof()),
            "read {i} after the pipe closed"
        );
    }
}
