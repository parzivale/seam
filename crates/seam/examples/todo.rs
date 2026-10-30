//! A to-do list in SQLite (`seam::sqlx`), with typed rows.
//!
//! The core runs SQL through `Execute` and `Fetch` and reads rows into its
//! own type through `TryFrom<&Row>`. The program keeps the list in a file;
//! the tests in an in-memory database, fresh for each.
//!
//! ```text
//! cargo run -p seam-rs --example todo --features std,test,sqlx,tokio,rt -- add "write examples"
//! cargo run -p seam-rs --example todo --features std,test,sqlx,tokio,rt -- done 1
//! cargo run -p seam-rs --example todo --features std,test,sqlx,tokio,rt -- list
//! ```

use seam::io;
use seam::prelude::*;
use seam::sqlx::{Execute, Fetch, Row, execute_send, fetch_all_as_send, fetch_one_as_send};

/// One item.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    id: i64,
    text: String,
    done: bool,
}

impl TryFrom<&Row> for Item {
    type Error = io::Error;
    fn try_from(row: &Row) -> io::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            text: row.try_get("text")?,
            done: row.try_get("done")?,
        })
    }
}

/// What to do.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Add(String),
    Done(i64),
    List,
}

/// Runs `command`; returns the list afterwards.
async fn run<C>(c: &mut C, command: Command) -> io::Result<Vec<Item>>
where
    C: for<'a> SendPerform<Execute<'a>> + for<'a> SendPerform<Fetch<'a>> + Send,
{
    // SQLite has no BOOLEAN through sqlx's `Any`: 0 and 1.
    execute_send(
        c,
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, text TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0)",
        &[],
    )
    .await?;
    match command {
        Command::Add(text) => {
            execute_send(c, "INSERT INTO items (text) VALUES (?)", &[text.into()]).await?;
        }
        Command::Done(id) => {
            if execute_send(c, "UPDATE items SET done = 1 WHERE id = ?", &[id.into()]).await? == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such item"));
            }
        }
        Command::List => {}
    }
    fetch_all_as_send(c, "SELECT id, text, done FROM items ORDER BY id", &[]).await
}

/// How many items are left to do.
async fn left<C>(c: &mut C) -> io::Result<u32>
where
    C: for<'a> SendPerform<Fetch<'a>> + Send,
{
    let (n,): (u32,) =
        fetch_one_as_send(c, "SELECT COUNT(*) FROM items WHERE done = 0", &[]).await?;
    Ok(n)
}

fn parse(args: &[String]) -> Option<Command> {
    match args {
        [] => Some(Command::List),
        [cmd] if cmd == "list" => Some(Command::List),
        [cmd, text @ ..] if cmd == "add" && !text.is_empty() => Some(Command::Add(text.join(" "))),
        [cmd, id] if cmd == "done" => id.parse().ok().map(Command::Done),
        _ => None,
    }
}

/// The edge: a database file, on Tokio.
fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = parse(&args) else {
        eprintln!("usage: todo [list | add TEXT | done ID]");
        std::process::exit(2);
    };
    seam::tokio::block_on(async {
        let mut db = seam::sqlx::Sqlx::connect("sqlite://todo.db?mode=rwc").await?;
        for item in run(&mut db, command).await? {
            println!(
                "{:>3} [{}] {}",
                item.id,
                if item.done { "x" } else { " " },
                item.text
            );
        }
        println!("{} left", left(&mut db).await?);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use seam::sqlx::Sqlx;

    use super::*;

    #[test]
    fn commands_parse() {
        let args = |s: &[&str]| s.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse(&args(&[])), Some(Command::List));
        assert_eq!(
            parse(&args(&["add", "buy", "milk"])),
            Some(Command::Add("buy milk".into()))
        );
        assert_eq!(parse(&args(&["done", "3"])), Some(Command::Done(3)));
        assert_eq!(parse(&args(&["done", "x"])), None);
    }

    #[test]
    fn add_then_finish() {
        seam::tokio::block_on(async {
            let mut db = Sqlx::memory().await.unwrap();
            run(&mut db, Command::Add("one".into())).await.unwrap();
            let items = run(&mut db, Command::Add("two".into())).await.unwrap();
            assert_eq!(items.len(), 2);
            let items = run(&mut db, Command::Done(1)).await.unwrap();
            assert_eq!(
                items[0],
                Item {
                    id: 1,
                    text: "one".into(),
                    done: true
                }
            );
            assert_eq!(left(&mut db).await.unwrap(), 1);
            let missing = run(&mut db, Command::Done(9)).await.unwrap_err();
            assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        });
    }
}
