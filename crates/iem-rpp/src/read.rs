//! Lossless RPP reader (S4 design note §3.2): the text is kept line for line,
//! so the exporter can patch single values and leave every other byte as
//! REAPER wrote it. `<HEAD …` opens a chunk, a line `>` closes it.

use crate::rpp::RppError;

fn invalid(msg: String) -> RppError {
    RppError::Invalid(msg)
}

/// The project text as lines (without line ends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub lines: Vec<String>,
    crlf: bool,
    final_eol: bool,
}

/// A chunk: the line indices of its head and its closing `>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Block {
    pub head: usize,
    pub end: usize,
    pub children: Vec<Block>,
}

/// A chunk's content in order: its own lines and its child chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    Line(usize),
    Chunk(&'a Block),
}

impl Block {
    /// Line indices inside this chunk that belong to no child chunk.
    pub fn direct(&self) -> Vec<usize> {
        self.items()
            .into_iter()
            .filter_map(|item| match item {
                Item::Line(i) => Some(i),
                Item::Chunk(_) => None,
            })
            .collect()
    }

    pub fn items(&self) -> Vec<Item<'_>> {
        let mut out = Vec::new();
        let mut kids = self.children.iter().peekable();
        let mut i = self.head + 1;
        while i < self.end {
            match kids.peek() {
                Some(child) if child.head == i => {
                    out.push(Item::Chunk(child));
                    i = child.end + 1;
                    kids.next();
                }
                _ => {
                    out.push(Item::Line(i));
                    i += 1;
                }
            }
        }
        out
    }
}

/// Splits a line into its tokens as written (quotes kept). A token that starts
/// with `"`, `'` or `` ` `` runs to the next same quote.
pub fn raw_tokens(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes.get(i).copied().unwrap_or(b' ');
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        let start = i;
        if matches!(c, b'"' | b'\'' | b'`') {
            i += 1;
            while i < bytes.len() && bytes.get(i) != Some(&c) {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
        } else {
            while i < bytes.len() && !matches!(bytes.get(i), Some(b' ' | b'\t')) {
                i += 1;
            }
        }
        out.push(s.get(start..i).unwrap_or_default());
    }
    out
}

/// Removes the quotes of one raw token.
pub fn unquote(token: &str) -> &str {
    let Some(first) = token.chars().next() else {
        return token;
    };
    if !matches!(first, '"' | '\'' | '`') {
        return token;
    }
    let inner = token.get(1..).unwrap_or_default();
    inner.strip_suffix(first).unwrap_or(inner)
}

/// The tokens of a line with their quotes removed.
pub fn tokens(s: &str) -> Vec<String> {
    raw_tokens(s)
        .into_iter()
        .map(|t| unquote(t).to_owned())
        .collect()
}

/// Parses a project into its lines and the chunk tree of the one top-level chunk.
pub fn parse(text: &str) -> Result<(Doc, Block), RppError> {
    let crlf = text.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let (body, final_eol) = match text.strip_suffix(eol) {
        Some(body) => (body, true),
        None => (text, false),
    };
    let lines: Vec<String> = body.split(eol).map(str::to_owned).collect();
    if let Some(n) = lines.iter().position(|l| l.contains(['\r', '\n'])) {
        return Err(invalid(format!("line {}: mixed line endings", n + 1)));
    }
    let mut stack: Vec<Block> = Vec::new();
    let mut root: Option<Block> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('<') {
            if root.is_some() && stack.is_empty() {
                return Err(invalid(format!("line {}: a second top-level chunk", i + 1)));
            }
            stack.push(Block {
                head: i,
                end: i,
                children: Vec::new(),
            });
        } else if t == ">" {
            let mut block = stack
                .pop()
                .ok_or_else(|| invalid(format!("line {}: '>' closes no chunk", i + 1)))?;
            block.end = i;
            match stack.last_mut() {
                Some(parent) => parent.children.push(block),
                None => root = Some(block),
            }
        } else if stack.is_empty() && !t.is_empty() {
            return Err(invalid(format!(
                "line {}: text outside the project chunk",
                i + 1
            )));
        }
    }
    if let Some(open) = stack.first() {
        return Err(invalid(format!(
            "the chunk opened at line {} is never closed",
            open.head + 1
        )));
    }
    let root = root.ok_or_else(|| invalid("no project chunk".to_owned()))?;
    Ok((
        Doc {
            lines,
            crlf,
            final_eol,
        },
        root,
    ))
}

impl Doc {
    pub fn render(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut out = self.lines.join(eol);
        if self.final_eol {
            out.push_str(eol);
        }
        out
    }

    /// A line without its indentation and trailing blanks.
    pub fn content(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", |l| l.trim())
    }

    pub fn tokens(&self, i: usize) -> Vec<String> {
        tokens(self.content(i))
    }

    /// The first token of a chunk head (`TRACK`, `VST`, …).
    pub fn chunk_name(&self, block: &Block) -> String {
        let head = self.content(block.head);
        tokens(head.strip_prefix('<').unwrap_or(head))
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    /// The tokens of a chunk head after `<`.
    pub fn head_tokens(&self, block: &Block) -> Vec<String> {
        let head = self.content(block.head);
        tokens(head.strip_prefix('<').unwrap_or(head))
    }

    /// Replaces a line's content, keeping its indentation.
    pub fn set_content(&mut self, i: usize, text: &str) -> Result<(), RppError> {
        let line = self
            .lines
            .get_mut(i)
            .ok_or_else(|| invalid(format!("line {} does not exist", i + 1)))?;
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        *line = format!("{indent}{text}");
        Ok(())
    }

    /// Replaces token `k` of line `i` (as written), keeping the others.
    pub fn set_token(&mut self, i: usize, k: usize, text: &str) -> Result<(), RppError> {
        let content = self.content(i).to_owned();
        let mut raw: Vec<&str> = raw_tokens(&content);
        let slot = raw
            .get_mut(k)
            .ok_or_else(|| invalid(format!("line {}: no token {k} in {content:?}", i + 1)))?;
        *slot = text;
        let joined = raw.join(" ");
        self.set_content(i, &joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "<REAPER_PROJECT 0.1 \"7.65/win64\" 0 0\n  RIPPLE 0 0\n  <TRACK {A}\n    NAME \"MIC 1\"\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      BYPASS 0 0 0\n    >\n  >\n  MASTER_VOLUME 1 0 -1 -1 1\n>\n";

    #[test]
    fn renders_byte_identical_with_lf_and_crlf() {
        let (doc, root) = parse(TEXT).unwrap();
        assert_eq!(doc.render(), TEXT);
        assert_eq!(root.head, 0);
        assert_eq!(root.end, 10);
        let crlf = TEXT.replace('\n', "\r\n");
        let (doc, _) = parse(&crlf).unwrap();
        assert_eq!(doc.render(), crlf);
        let no_final = TEXT.trim_end_matches('\n');
        let (doc, _) = parse(no_final).unwrap();
        assert_eq!(doc.render(), no_final);
    }

    #[test]
    fn builds_the_chunk_tree_and_direct_lines() {
        let (doc, root) = parse(TEXT).unwrap();
        assert_eq!(root.children.len(), 1);
        let track = &root.children[0];
        assert_eq!((track.head, track.end), (2, 8));
        assert_eq!(doc.chunk_name(track), "TRACK");
        assert_eq!(
            doc.head_tokens(&root),
            vec!["REAPER_PROJECT", "0.1", "7.65/win64", "0", "0"]
        );
        assert_eq!(track.direct(), vec![3, 4]);
        assert_eq!(root.direct(), vec![1, 9]);
        let fx = &track.children[0];
        assert_eq!(doc.chunk_name(fx), "FXCHAIN");
        assert_eq!(fx.items(), vec![Item::Line(6)]);
        assert_eq!(
            track.items(),
            vec![Item::Line(3), Item::Line(4), Item::Chunk(fx)]
        );
        assert_eq!(doc.tokens(3), vec!["NAME", "MIC 1"]);
        assert_eq!(doc.content(4), "VOLPAN 1 0 -1 -1 1");
        assert_eq!(doc.content(99), "");
    }

    #[test]
    fn unbalanced_or_stray_text_and_mixed_line_ends_are_errors() {
        for bad in [
            "<A\n",
            "<A\n>\n>\n",
            "x\n<A\n>\n",
            "<A\n>\ny\n",
            "<A\n>\n<B\n>\n",
            "<A\r\n  X\n>\r\n",
            "",
            "\n\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(parse("<A\n>\n\n").is_ok());
    }

    #[test]
    fn tokens_follow_reaper_quoting() {
        assert_eq!(
            raw_tokens("AUXRECV 0 3 1 0 0 0 0 0 0 -1:U 0 -1 ''"),
            vec![
                "AUXRECV", "0", "3", "1", "0", "0", "0", "0", "0", "0", "-1:U", "0", "-1", "''"
            ]
        );
        assert_eq!(
            tokens("NAME \"a b\" 'c \"d\"' `e 'f'` g\th"),
            vec!["NAME", "a b", "c \"d\"", "e 'f'", "g", "h"]
        );
        assert_eq!(tokens("X \"open"), vec!["X", "open"]);
        assert_eq!(unquote("\"\""), "");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote(""), "");
        assert_eq!(raw_tokens("   "), Vec::<&str>::new());
    }

    #[test]
    fn set_token_keeps_indentation_and_other_tokens() {
        let (mut doc, _) = parse(TEXT).unwrap();
        doc.set_token(4, 1, "0.5").unwrap();
        assert_eq!(doc.lines[4], "    VOLPAN 0.5 0 -1 -1 1");
        doc.set_token(3, 1, "\"MIC 2\"").unwrap();
        assert_eq!(doc.lines[3], "    NAME \"MIC 2\"");
        assert!(doc.set_token(4, 9, "x").is_err());
        assert!(doc.set_token(99, 0, "x").is_err());
        doc.set_content(6, "BYPASS 1 0 0").unwrap();
        assert_eq!(doc.lines[6], "      BYPASS 1 0 0");
        assert!(doc.set_content(99, "x").is_err());
    }
}
