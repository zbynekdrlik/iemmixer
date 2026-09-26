//! RPP text writer. A REAPER project is line based: `<HEAD …` opens a
//! chunk, `>` closes it, every other line is space-separated tokens.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum RppError {
    #[error("value is not finite: {0}")]
    NotFinite(f64),
    #[error("string cannot be quoted for RPP: {0:?}")]
    Unquotable(String),
    #[error("invalid project: {0}")]
    Invalid(String),
}

/// Quotes one token the way REAPER does: bare when non-empty without space
/// or quote characters, else the first of `"`, `'`, `` ` `` it does not contain.
pub fn q(s: &str) -> Result<String, RppError> {
    if s.contains(['\n', '\r']) {
        return Err(RppError::Unquotable(s.to_owned()));
    }
    if !s.is_empty() && !s.contains([' ', '"', '\'', '`']) {
        return Ok(s.to_owned());
    }
    ['"', '\'', '`']
        .into_iter()
        .find(|quote| !s.contains(*quote))
        .map(|quote| format!("{quote}{s}{quote}"))
        .ok_or_else(|| RppError::Unquotable(s.to_owned()))
}

/// Shortest decimal that parses back to the same f64 (Rust's `Display`
/// never uses exponent notation). `-0` is written as `0`.
pub fn num(x: f64) -> Result<String, RppError> {
    if !x.is_finite() {
        return Err(RppError::NotFinite(x));
    }
    if x == 0.0 {
        return Ok("0".to_owned());
    }
    Ok(format!("{x}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Line(String),
    Chunk(Chunk),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub head: String,
    pub body: Vec<Node>,
}

impl Chunk {
    pub fn new(head: impl Into<String>) -> Self {
        Self {
            head: head.into(),
            body: Vec::new(),
        }
    }

    pub fn line(&mut self, text: impl Into<String>) -> &mut Self {
        self.body.push(Node::Line(text.into()));
        self
    }

    pub fn child(&mut self, chunk: Chunk) -> &mut Self {
        self.body.push(Node::Chunk(chunk));
        self
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out
    }

    fn render_into(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        out.push_str(&pad);
        out.push('<');
        out.push_str(&self.head);
        out.push('\n');
        for node in &self.body {
            match node {
                Node::Line(text) => {
                    out.push_str(&pad);
                    out.push_str("  ");
                    out.push_str(text);
                    out.push('\n');
                }
                Node::Chunk(chunk) => chunk.render_into(out, depth + 1),
            }
        }
        out.push_str(&pad);
        out.push_str(">\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_tokens_stay_bare_and_others_are_quoted() {
        assert_eq!(q("reaeq.dll").unwrap(), "reaeq.dll");
        assert_eq!(q("").unwrap(), "\"\"");
        assert_eq!(q("a b").unwrap(), "\"a b\"");
        assert_eq!(q("say \"hi\"").unwrap(), "'say \"hi\"'");
        assert_eq!(q("it's \"x\"").unwrap(), "`it's \"x\"`");
        assert!(q("a\"b'c`d").is_err());
        assert!(q("two\nlines").is_err());
    }

    #[test]
    fn numbers_round_trip_without_exponents() {
        assert_eq!(num(1.0).unwrap(), "1");
        assert_eq!(num(-0.0).unwrap(), "0");
        assert_eq!(num(0.1).unwrap(), "0.1");
        assert_eq!(num(1e-7).unwrap(), "0.0000001");
        assert_eq!(num(0.000803).unwrap(), "0.000803");
        assert_eq!(num(-0.4).unwrap(), "-0.4");
        let x = 2996.2342070275295_f64;
        assert_eq!(
            num(x).unwrap().parse::<f64>().unwrap().to_bits(),
            x.to_bits()
        );
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(num(bad).is_err());
        }
    }

    #[test]
    fn chunks_render_nested_with_two_space_indent() {
        let mut inner = Chunk::new("SOURCE WAVE");
        inner.line("FILE \"x.wav\"");
        let mut outer = Chunk::new("ITEM");
        outer.line("POSITION 0").child(inner);
        assert_eq!(
            outer.render(),
            "<ITEM\n  POSITION 0\n  <SOURCE WAVE\n    FILE \"x.wav\"\n  >\n>\n"
        );
    }
}
