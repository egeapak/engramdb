#![no_main]

use engramdb::daemon::protocol::{read_msg, DaemonRequest, DaemonResponse};
use libfuzzer_sys::fuzz_target;

// The daemon reads newline-delimited JSON frames from any process of the
// user that can reach its socket, and a client reads the daemon's replies.
// Any byte stream must come back as a frame, a clean EOF or an error.
//
// The 64 MiB frame cap is far beyond libFuzzer's input sizes; it stays
// covered by the unit test of `read_msg_capped`.

thread_local! {
    static RT: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
}

fuzz_target!(|data: &[u8]| {
    RT.with(|rt| {
        rt.block_on(async {
            let mut reader = tokio::io::BufReader::new(data);
            // Read frames until EOF or the first error, as a server loop does.
            for _ in 0..64 {
                match read_msg::<_, DaemonRequest>(&mut reader).await {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
            let mut reader = tokio::io::BufReader::new(data);
            for _ in 0..64 {
                match read_msg::<_, DaemonResponse>(&mut reader).await {
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                }
            }
        })
    });
});
