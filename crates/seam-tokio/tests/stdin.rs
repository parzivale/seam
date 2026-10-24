//! Reading real standard input: the test runs itself as a child process,
//! with its stdin piped.
#![cfg(feature = "io-std")]

use std::io::Write as _;
use std::process::{Command, Stdio};

use seam_core::AsyncAcquire;
use seam_core::io;
use seam_core::ops::io::{BufReader, Line};
use seam_core::ops::stdio::OpenStdin;

use seam_tokio::Tokio;

const CHILD: &str = "SEAM_STDIN_CHILD";

#[tokio::test]
async fn lines_from_real_stdin() {
    if std::env::var_os(CHILD).is_some() {
        let mut tokio = Tokio::new();
        // One reader across both scopes: it keeps what it read past a line.
        let mut reader = BufReader::<512>::new();
        let mut buf = [0; 700];
        let first = tokio
            .acquire_async(OpenStdin::new(), async |_, stdin| {
                reader.read_line_send(stdin, &mut buf).await
            })
            .await
            .unwrap()
            .unwrap();
        println!("FIRST {}", first.len);
        tokio
            .acquire_async(OpenStdin::new(), async |_, stdin| {
                while let Some(Line { len, complete }) =
                    reader.read_line_send(stdin, &mut buf).await?
                {
                    let last = buf[len - 1];
                    let last = if last == b'\n' {
                        "nl".to_owned()
                    } else {
                        (last as char).to_string()
                    };
                    println!("LINE {len} {complete} {last}");
                }
                io::Result::Ok(())
            })
            .await
            .unwrap();
        println!("END");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lines_from_real_stdin", "--nocapture"])
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
            "FIRST 3",
            "LINE 700 false x",
            "LINE 601 true nl",
            "LINE 4 true l",
            "END",
        ]
    );
}
