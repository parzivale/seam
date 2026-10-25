//! A parser for the subset of KDL v2 that world files use.
//!
//! Supported: nodes with arguments, properties and children; `;` and line
//! endings as terminators; line continuations (`\`); `//` and nested `/*`
//! comments; slashdash (`/-`) on nodes, arguments, properties and child
//! blocks; quoted strings with escapes, including whitespace escapes;
//! multiline strings (`"""`) with KDL's dedenting; raw strings (`#"..."#`,
//! `#"""..."""#`); identifier strings; integers (decimal, `0x`, `0o`,
//! `0b`, with `_` separators); and `#true`, `#false`, `#null`.
//!
//! Not supported, with a clear error: type annotations, and fractional
//! or exponent numbers.

use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

/// Where something is in the source, 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pos {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

/// A node of a world file: a name, arguments, properties and a block of
/// children. Read it through its methods, in [`world_file`](crate::world_file).
#[derive(Debug)]
pub struct Node {
    pub(crate) name: String,
    pub(crate) pos: Pos,
    pub(crate) args: Vec<Value>,
    /// In source order; a repeated key's last value wins.
    pub(crate) props: Vec<(String, Value)>,
    pub(crate) children: Vec<Node>,
}

/// An argument or property value in a world file: a string, an integer, a
/// boolean or `#null`. Read it through its methods, in
/// [`world_file`](crate::world_file).
#[derive(Debug, Clone)]
pub struct Value {
    pub(crate) kind: Kind,
    pub(crate) pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    Str(String),
    Int(i128),
    Bool(bool),
    Null,
}

#[derive(Debug)]
pub(crate) struct ParseError {
    pub(crate) pos: Pos,
    pub(crate) message: String,
}

/// Parses a document into its top-level nodes.
pub(crate) fn parse(text: &str) -> Result<Vec<Node>, ParseError> {
    let mut p = Parser {
        chars: text.chars().collect(),
        i: 0,
        line: 1,
        column: 1,
    };
    let nodes = p.nodes(false)?;
    Ok(nodes)
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    line: usize,
    column: usize,
}

/// Characters that cannot appear in an identifier.
fn is_special(c: char) -> bool {
    matches!(
        c,
        '\\' | '/' | '(' | ')' | '{' | '}' | '[' | ']' | ';' | '=' | '"' | '#'
    )
}

fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{85}' | '\u{0B}' | '\u{0C}' | '\u{2028}' | '\u{2029}'
    )
}

fn is_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{FEFF}' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

fn is_ident_char(c: char) -> bool {
    !is_space(c) && !is_newline(c) && !is_special(c) && !c.is_control()
}

impl Parser {
    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            column: self.column,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.i + n).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(n, c)| self.peek_at(n) == Some(c))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\r' && self.peek() == Some('\n') {
            // CRLF: one line break, counted on the '\n'.
            self.column += 1;
        } else if is_newline(c) {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(c)
    }

    fn err<T>(&self, pos: Pos, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            pos,
            message: message.into(),
        })
    }

    /// Skips a `/* */` comment, which may nest.
    fn block_comment(&mut self) -> Result<(), ParseError> {
        let start = self.pos();
        self.bump();
        self.bump();
        let mut depth = 1;
        while depth > 0 {
            if self.starts_with("/*") {
                self.bump();
                self.bump();
                depth += 1;
            } else if self.starts_with("*/") {
                self.bump();
                self.bump();
                depth -= 1;
            } else if self.bump().is_none() {
                return self.err(start, "unterminated `/*` comment");
            }
        }
        Ok(())
    }

    fn line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if is_newline(c) {
                break;
            }
            self.bump();
        }
    }

    /// Skips spaces, block comments and line continuations: what may
    /// separate the parts of one node.
    fn node_space(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek() {
                Some(c) if is_space(c) => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('*') => self.block_comment()?,
                Some('\\') => {
                    // A line continuation: `\`, then spaces or a comment, then a newline.
                    let pos = self.pos();
                    self.bump();
                    loop {
                        match self.peek() {
                            Some(c) if is_space(c) => {
                                self.bump();
                            }
                            Some('/') if self.peek_at(1) == Some('*') => self.block_comment()?,
                            _ => break,
                        }
                    }
                    if self.starts_with("//") {
                        self.line_comment();
                    }
                    match self.peek() {
                        Some(c) if is_newline(c) => {
                            self.bump();
                        }
                        None => {}
                        _ => {
                            return self.err(
                                pos,
                                "`\\` continues a line, so only a newline may follow it",
                            );
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Skips whitespace, newlines, comments and `;`: what may separate
    /// nodes.
    fn line_space(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek() {
                Some(c) if is_space(c) || is_newline(c) || c == ';' => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => self.line_comment(),
                Some('/') if self.peek_at(1) == Some('*') => self.block_comment()?,
                _ => return Ok(()),
            }
        }
    }

    /// Parses nodes until the end of input, or until `}` if `in_block`.
    fn nodes(&mut self, in_block: bool) -> Result<Vec<Node>, ParseError> {
        let mut nodes = Vec::new();
        loop {
            self.line_space()?;
            match self.peek() {
                None if in_block => return self.err(self.pos(), "missing `}`"),
                None => return Ok(nodes),
                Some('}') if in_block => return Ok(nodes),
                Some('}') => return self.err(self.pos(), "unexpected `}`"),
                _ => {}
            }
            let dashed = self.slashdash()?;
            let node = self.node()?;
            if !dashed {
                nodes.push(node);
            }
        }
    }

    /// Consumes a `/-` and the space after it; whether there was one.
    fn slashdash(&mut self) -> Result<bool, ParseError> {
        if self.starts_with("/-") {
            self.bump();
            self.bump();
            self.line_space()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn node(&mut self) -> Result<Node, ParseError> {
        let pos = self.pos();
        if self.peek() == Some('(') {
            return self.err(pos, "type annotations are not supported in world files");
        }
        let name = match self.value()? {
            Value {
                kind: Kind::Str(s), ..
            } => s,
            v => return self.err(v.pos, "a node's name must be a string or an identifier"),
        };
        let mut node = Node {
            name,
            pos,
            args: Vec::new(),
            props: Vec::new(),
            children: Vec::new(),
        };
        let mut had_children = false;
        loop {
            let before = self.i;
            self.node_space()?;
            let spaced = self.i > before;
            match self.peek() {
                None => return Ok(node),
                Some(c) if is_newline(c) || c == ';' => {
                    self.bump();
                    return Ok(node);
                }
                Some('}') => return Ok(node),
                Some('/') if self.peek_at(1) == Some('/') => {
                    self.line_comment();
                    return Ok(node);
                }
                _ => {}
            }
            let dashed = self.slashdash()?;
            if self.peek() == Some('{') {
                let block_pos = self.pos();
                if had_children && !dashed {
                    return self.err(block_pos, "a node has at most one block of children");
                }
                self.bump();
                let children = self.nodes(true)?;
                self.bump(); // the `}`
                if !dashed {
                    node.children = children;
                    had_children = true;
                }
                continue;
            }
            if had_children {
                return self.err(
                    self.pos(),
                    "arguments and properties go before a node's children",
                );
            }
            if !spaced && !dashed {
                return self.err(self.pos(), "expected a space before this");
            }
            let start = self.pos();
            let first = self.value()?;
            if self.peek() == Some('=') {
                let key = match first.kind {
                    Kind::Str(k) => k,
                    _ => return self.err(start, "a property's name must be a string"),
                };
                self.bump();
                if self.peek().is_some_and(|c| is_space(c) || is_newline(c)) {
                    return self.err(self.pos(), "no space allowed after `=`");
                }
                let value = self.value()?;
                if !dashed {
                    node.props.push((key, value));
                }
            } else if !dashed {
                node.args.push(first);
            }
        }
    }

    /// A string, identifier, number, or keyword.
    fn value(&mut self) -> Result<Value, ParseError> {
        let pos = self.pos();
        let kind = match self.peek() {
            Some('"') => Kind::Str(self.quoted()?),
            Some('#') if matches!(self.peek_at(1), Some('#' | '"')) => Kind::Str(self.raw()?),
            Some('#') => self.keyword()?,
            Some(c) if c.is_ascii_digit() => Kind::Int(self.number()?),
            Some('+' | '-') if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => {
                Kind::Int(self.number()?)
            }
            Some('(') => return self.err(pos, "type annotations are not supported in world files"),
            Some('/') => {
                return self.err(
                    pos,
                    "a bare word cannot contain `/` in KDL: quote paths, as in \"/home/ada\"",
                );
            }
            Some(c) if is_ident_char(c) => {
                let id = self.identifier();
                if matches!(
                    id.as_str(),
                    "true" | "false" | "null" | "inf" | "-inf" | "nan"
                ) {
                    return self.err(
                        pos,
                        format!("write `#{id}`: a bare `{id}` is not a value in KDL"),
                    );
                }
                Kind::Str(id)
            }
            Some(c) => return self.err(pos, format!("unexpected `{c}`")),
            None => return self.err(pos, "unexpected end of input"),
        };
        Ok(Value { kind, pos })
    }

    fn identifier(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if !is_ident_char(c) {
                break;
            }
            s.push(c);
            self.bump();
        }
        s
    }

    fn keyword(&mut self) -> Result<Kind, ParseError> {
        let pos = self.pos();
        self.bump(); // '#'
        let word = self.identifier();
        match word.as_str() {
            "true" => Ok(Kind::Bool(true)),
            "false" => Ok(Kind::Bool(false)),
            "null" => Ok(Kind::Null),
            "inf" | "-inf" | "nan" => self.err(pos, "only integers are supported in world files"),
            _ => self.err(pos, format!("unknown keyword `#{word}`")),
        }
    }

    fn number(&mut self) -> Result<i128, ParseError> {
        let pos = self.pos();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if !is_ident_char(c) {
                break;
            }
            text.push(c);
            self.bump();
        }
        let (neg, body) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(&text)),
        };
        let (radix, digits) = match body.get(..2) {
            Some("0x") => (16, &body[2..]),
            Some("0o") => (8, &body[2..]),
            Some("0b") => (2, &body[2..]),
            _ => (10, body),
        };
        if radix == 10 && digits.contains(['.', 'e', 'E']) {
            return self.err(pos, "only integers are supported in world files");
        }
        let clean: String = digits.chars().filter(|&c| c != '_').collect();
        if clean.is_empty() || digits.starts_with('_') {
            return self.err(pos, format!("`{text}` is not a number"));
        }
        match i128::from_str_radix(&clean, radix) {
            Ok(n) => Ok(if neg { -n } else { n }),
            Err(_) => self.err(pos, format!("`{text}` is not a number")),
        }
    }

    /// A `"..."` or `"""..."""` string.
    fn quoted(&mut self) -> Result<String, ParseError> {
        let pos = self.pos();
        if self.starts_with("\"\"\"") {
            for _ in 0..3 {
                self.bump();
            }
            let raw = self.multiline_body(pos, "\"\"\"", false)?;
            return Ok(raw);
        }
        self.bump();
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return self.err(pos, "unterminated string"),
                Some('"') => return Ok(s),
                Some(c) if is_newline(c) => {
                    return self.err(
                        pos,
                        "a `\"` string cannot span lines; use `\"\"\"` for that",
                    );
                }
                Some('\\') => self.escape(&mut s)?,
                Some(c) => s.push(c),
            }
        }
    }

    /// The part of an escape after the `\`.
    fn escape(&mut self, s: &mut String) -> Result<(), ParseError> {
        let pos = self.pos();
        match self.bump() {
            Some('n') => s.push('\n'),
            Some('r') => s.push('\r'),
            Some('t') => s.push('\t'),
            Some('\\') => s.push('\\'),
            Some('"') => s.push('"'),
            Some('b') => s.push('\u{08}'),
            Some('f') => s.push('\u{0C}'),
            Some('s') => s.push(' '),
            Some('u') => {
                if self.bump() != Some('{') {
                    return self.err(pos, "expected `{` after `\\u`");
                }
                let mut hex = String::new();
                while let Some(c) = self.peek() {
                    if c == '}' {
                        break;
                    }
                    hex.push(c);
                    self.bump();
                }
                self.bump();
                let c = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                match c {
                    Some(c) => s.push(c),
                    None => return self.err(pos, format!("`\\u{{{hex}}}` is not a character")),
                }
            }
            // A whitespace escape: the backslash and all whitespace after it vanish.
            Some(c) if is_space(c) || is_newline(c) => {
                while self.peek().is_some_and(|c| is_space(c) || is_newline(c)) {
                    self.bump();
                }
            }
            Some(c) => return self.err(pos, format!("unknown escape `\\{c}`")),
            None => return self.err(pos, "unterminated string"),
        }
        Ok(())
    }

    /// A `#"..."#` or `#"""..."""#` string.
    fn raw(&mut self) -> Result<String, ParseError> {
        let pos = self.pos();
        let mut hashes = 0;
        while self.peek() == Some('#') {
            self.bump();
            hashes += 1;
        }
        let close: String = core::iter::once('"')
            .chain(core::iter::repeat_n('#', hashes))
            .collect();
        if self.starts_with("\"\"\"") {
            for _ in 0..3 {
                self.bump();
            }
            let close: String = "\"\"\""
                .chars()
                .chain(core::iter::repeat_n('#', hashes))
                .collect();
            return self.multiline_body(pos, &close, true);
        }
        if self.bump() != Some('"') {
            return self.err(pos, "expected `\"` after `#`");
        }
        let mut s = String::new();
        loop {
            if self.starts_with(&close) {
                for _ in 0..close.chars().count() {
                    self.bump();
                }
                return Ok(s);
            }
            match self.bump() {
                None => return self.err(pos, "unterminated raw string"),
                Some(c) if is_newline(c) => {
                    return self.err(
                        pos,
                        "a raw string cannot span lines; use `#\"\"\"` for that",
                    );
                }
                Some(c) => s.push(c),
            }
        }
    }

    /// The body of a multiline string, after its opening quotes: lines up to
    /// the one holding `close`, dedented by that line's indentation, with
    /// the first and last line breaks dropped. Escapes are then processed
    /// unless `raw`.
    fn multiline_body(&mut self, pos: Pos, close: &str, raw: bool) -> Result<String, ParseError> {
        // The opening quotes must end their line.
        while self.peek().is_some_and(is_space) {
            self.bump();
        }
        match self.peek() {
            Some(c) if is_newline(c) => {
                self.bump();
                if c == '\r' && self.peek() == Some('\n') {
                    self.bump();
                }
            }
            _ => {
                return self.err(
                    pos,
                    "a multiline string starts on the line after its `\"\"\"`",
                );
            }
        }
        let mut lines: Vec<String> = Vec::new();
        loop {
            // Read one line, watching for the closing quotes.
            let mut line = String::new();
            loop {
                if self.starts_with(close) && line.chars().all(is_space) {
                    for _ in 0..close.chars().count() {
                        self.bump();
                    }
                    let indent = line;
                    let mut out = String::new();
                    for (n, l) in lines.iter().enumerate() {
                        if n > 0 {
                            out.push('\n');
                        }
                        if l.chars().all(is_space) {
                            continue; // whitespace-only lines become empty
                        }
                        match l.strip_prefix(indent.as_str()) {
                            Some(rest) => out.push_str(rest),
                            None => {
                                return self.err(
                                    Pos {
                                        line: pos.line + 1 + n,
                                        column: 1,
                                    },
                                    "this line is indented less than the closing `\"\"\"`",
                                );
                            }
                        }
                    }
                    if raw {
                        return Ok(out);
                    }
                    return unescape(&out).map_err(|message| ParseError { pos, message });
                }
                match self.peek() {
                    None => return self.err(pos, "unterminated multiline string"),
                    Some(c) if is_newline(c) => {
                        self.bump();
                        if c == '\r' && self.peek() == Some('\n') {
                            self.bump();
                        }
                        break;
                    }
                    Some(c) => {
                        line.push(c);
                        self.bump();
                    }
                }
            }
            lines.push(line);
        }
    }
}

/// Processes escapes in an already dedented multiline string.
fn unescape(s: &str) -> Result<String, String> {
    let mut p = Parser {
        chars: s.chars().collect(),
        i: 0,
        line: 1,
        column: 1,
    };
    let mut out = String::new();
    while let Some(c) = p.bump() {
        if c == '\\' {
            p.escape(&mut out).map_err(|e| e.message)?;
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

impl Kind {
    /// What this kind of value is called, for errors.
    pub(crate) fn describe(&self) -> String {
        match self {
            Kind::Str(_) => "a string".to_string(),
            Kind::Int(_) => "an integer".to_string(),
            Kind::Bool(_) => "a boolean".to_string(),
            Kind::Null => "`#null`".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &Value) -> &str {
        match &v.kind {
            Kind::Str(s) => s,
            k => panic!("not a string: {k:?}"),
        }
    }

    #[test]
    fn nodes_args_props_children() {
        let doc = parse(
            "a 1 \"two\" three key=#true /-skipped other=0x1F {\n  b; c \"x\"\n}\n// comment\nd /* inline */ -5_000\n/-e { gone }\n",
        )
        .unwrap();
        assert_eq!(doc.len(), 2);
        let a = &doc[0];
        assert_eq!(a.name, "a");
        assert_eq!(a.args.len(), 3);
        assert_eq!(a.args[0].kind, Kind::Int(1));
        assert_eq!(s(&a.args[1]), "two");
        assert_eq!(s(&a.args[2]), "three");
        assert_eq!(a.props[0].0, "key");
        assert_eq!(a.props[0].1.kind, Kind::Bool(true));
        assert_eq!(a.props[1].1.kind, Kind::Int(31));
        assert_eq!(a.children.len(), 2);
        assert_eq!(s(&a.children[1].args[0]), "x");
        assert_eq!(doc[1].args[0].kind, Kind::Int(-5000));
        assert_eq!(doc[1].pos, Pos { line: 5, column: 1 });
    }

    #[test]
    fn strings() {
        let doc = parse(concat!(
            "a \"tab\\tnewline\\n\\u{1F600}\" #\"raw \\n \"quoted\"\"# \"one \\\n     line\"\n",
            "b \"\"\"\n    first\n      indented\n\n    last\n    \"\"\"\n",
            "c #\"\"\"\n  raw \\n\n  \"\"\"#\n",
        ))
        .unwrap();
        assert_eq!(s(&doc[0].args[0]), "tab\tnewline\n\u{1F600}");
        assert_eq!(s(&doc[0].args[1]), "raw \\n \"quoted\"");
        assert_eq!(s(&doc[0].args[2]), "one line");
        assert_eq!(s(&doc[1].args[0]), "first\n  indented\n\nlast");
        assert_eq!(s(&doc[2].args[0]), "raw \\n");
    }

    #[test]
    fn line_continuations() {
        let doc = parse("a 1 \\ // why not\n  2\nb").unwrap();
        assert_eq!(doc[0].args.len(), 2);
        assert_eq!(doc[1].name, "b");
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse("a \"open\n").unwrap_err();
        assert_eq!(e.pos, Pos { line: 1, column: 3 });
        let e = parse("a {\n  b\n").unwrap_err();
        assert!(e.message.contains("missing `}`"), "{}", e.message);
        let e = parse("a true").unwrap_err();
        assert!(e.message.contains("#true"), "{}", e.message);
        let e = parse("a 1.5").unwrap_err();
        assert!(e.message.contains("integers"), "{}", e.message);
        let e = parse("a (u8)1").unwrap_err();
        assert!(e.message.contains("type annotations"), "{}", e.message);
        let e = parse("a \"\"\"\n  x\n y\n  \"\"\"").unwrap_err();
        assert_eq!(e.pos.line, 3);
    }
}
