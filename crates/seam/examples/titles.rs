//! Page titles over HTTP (`seam::reqwest`): fetch each page, report its
//! status and `<title>`.
//!
//! The core sends requests through `Fetch`, so the program uses reqwest
//! while the tests use `Scripted`, which answers from routes they set and
//! records what was asked.
//!
//! ```text
//! cargo run -p seam-rs --example titles --features std,test,reqwest,tokio,rt -- https://example.com
//! ```

use seam::io;
use seam::prelude::*;
use seam::reqwest::{Fetch, get_send};

/// A page's status, and its title if it has one.
#[derive(Debug, PartialEq, Eq)]
struct Page {
    url: String,
    status: u16,
    title: Option<String>,
}

/// The text of the first `<title>`, tags and spacing removed.
fn title_of(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title>")?;
    let title = html[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!title.is_empty()).then_some(title)
}

/// Fetches each URL in turn.
async fn titles<C>(c: &mut C, urls: &[String]) -> Vec<io::Result<Page>>
where
    C: SendPerform<Fetch> + Send,
{
    let mut pages = Vec::new();
    for url in urls {
        let page = get_send(c, url).await.map(|response| Page {
            url: url.clone(),
            status: response.status().as_u16(),
            title: title_of(&String::from_utf8_lossy(response.body())),
        });
        pages.push(page);
    }
    pages
}

/// The edge: reqwest, on Tokio.
fn main() {
    let mut urls: Vec<String> = std::env::args().skip(1).collect();
    if urls.is_empty() {
        urls.push("https://www.rust-lang.org".into());
    }
    let mut http = seam::reqwest::Reqwest::new();
    for page in seam::tokio::block_on(titles(&mut http, &urls)) {
        match page {
            Ok(p) => println!(
                "{} {} {}",
                p.status,
                p.url,
                p.title.as_deref().unwrap_or("(no title)")
            ),
            Err(e) => println!("--- {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use seam::reqwest::Scripted;

    use super::*;

    #[test]
    fn titles_are_found_whatever_the_markup() {
        assert_eq!(
            title_of("<html><TITLE>\n  Hello,\n world </TITLE>"),
            Some("Hello, world".into())
        );
        assert_eq!(title_of("<title lang=en>Hi</title>"), Some("Hi".into()));
        assert_eq!(title_of("<title></title>"), None);
        assert_eq!(title_of("no title"), None);
    }

    #[test]
    fn each_page_reports_its_status() {
        let mut http = Scripted::new();
        http.respond("GET", "https://a.test/", 200, "<title>A</title>")
            .respond(
                "GET",
                "https://b.test/gone",
                404,
                "<title>Not Found</title>",
            );
        let urls = [
            "https://a.test/".to_owned(),
            "https://b.test/gone".to_owned(),
        ];
        let pages = seam::future::block_on(titles(&mut http, &urls));
        let pages: Vec<Page> = pages.into_iter().map(Result::unwrap).collect();
        assert_eq!(
            pages[0],
            Page {
                url: urls[0].clone(),
                status: 200,
                title: Some("A".into())
            }
        );
        assert_eq!(pages[1].status, 404);
        assert_eq!(http.requests().len(), 2);
    }
}
