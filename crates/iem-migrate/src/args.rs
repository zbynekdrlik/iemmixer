//! A small flag parser: `--name value` options and `--switch` flags, each
//! at most once; anything else is a usage error.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::{Failure, USAGE};

#[derive(Debug, Default)]
pub struct Args {
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

pub fn parse(args: &[String], options: &[&str], switches: &[&str]) -> Result<Args, Failure> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let dup = || Failure::input(format!("{a} given twice"));
        if options.contains(&a.as_str()) {
            let v = it
                .next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| Failure::input(format!("{a} needs a value\n\n{USAGE}")))?;
            if out.values.insert(a.clone(), v.clone()).is_some() {
                return Err(dup());
            }
        } else if switches.contains(&a.as_str()) {
            if !out.switches.insert(a.clone()) {
                return Err(dup());
            }
        } else {
            return Err(Failure::input(format!("unknown argument {a:?}\n\n{USAGE}")));
        }
    }
    Ok(out)
}

impl Args {
    pub fn opt(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn req(&self, name: &str) -> Result<&str, Failure> {
        self.opt(name)
            .ok_or_else(|| Failure::input(format!("{name} is required\n\n{USAGE}")))
    }

    pub fn path(&self, name: &str) -> Result<PathBuf, Failure> {
        self.req(name).map(PathBuf::from)
    }

    pub fn opt_path(&self, name: &str) -> Option<PathBuf> {
        self.opt(name).map(PathBuf::from)
    }

    pub fn flag(&self, name: &str) -> bool {
        self.switches.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn options_and_switches_parse_once() {
        let a = parse(&v(&["--a", "1", "--s"]), &["--a", "--b"], &["--s", "--t"]).unwrap();
        assert_eq!(a.opt("--a"), Some("1"));
        assert_eq!(a.req("--a"), Ok("1"));
        assert_eq!(a.path("--a").unwrap(), PathBuf::from("1"));
        assert_eq!(a.opt("--b"), None);
        assert_eq!(a.opt_path("--b"), None);
        assert!(a.req("--b").unwrap_err().msg.starts_with("--b is required"));
        assert!(a.flag("--s"));
        assert!(!a.flag("--t"));
        for bad in [
            v(&["--a"]),
            v(&["--a", "--s"]),
            v(&["--a", "1", "--a", "2"]),
            v(&["--s", "--s"]),
            v(&["--x"]),
            v(&["x"]),
        ] {
            let e = parse(&bad, &["--a"], &["--s"]).unwrap_err();
            assert_eq!(e.code, crate::EXIT_INPUT, "{bad:?}");
        }
    }
}
