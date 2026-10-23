//! Reading real standard input: the test runs itself as a child process,
//! with its stdin piped.
#![cfg(feature = "io-std")]

use std::io::Write as _;
use std::process::{Command, Stdio};

use seam_core::future::SendPerform;
use seam_core::ops::stdio::{Line, ReadLine, ReadLineInto};
use seam_tokio::Tokio;

const CHILD: &str = "SEAM_STDIN_CHILD";

#[tokio::test]
async fn read_line_into_shares_the_stdin_buffer() {
    if std::env::var_os(CHILD).is_some() {
        let mut tokio = Tokio::new();
        // Through the same buffer as `ReadLine`: nothing is lost between them.
        // A line comes back in several blocking reads.
        let first = tokio.perform_send(ReadLine).await.unwrap();
        println!("FIRST {first:?}");
        let mut buf = [0; 700];
        while let Some(Line { len, complete }) =
            tokio.perform_send(ReadLineInto(&mut buf)).await.unwrap()
        {
            let last = buf[len - 1];
            let last = if last == b'\n' {
                "nl".to_owned()
            } else {
                (last as char).to_string()
            };
            println!("LINE {len} {complete} {last}");
        }
        println!("END");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "read_line_into_shares_the_stdin_buffer",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = b"ab\n".to_vec();
    input.extend([b'x'; 1300]);
    input.extend(b"\ntail");
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<_> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| ["FIRST", "LINE", "END"].iter().any(|p| l.starts_with(p)))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines,
        [
            "FIRST Some(\"ab\\n\")",
            "LINE 700 false x",
            "LINE 601 true nl",
            "LINE 4 true l",
            "END",
        ]
    );
}
