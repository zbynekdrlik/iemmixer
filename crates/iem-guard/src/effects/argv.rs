//! Arguments of the commands `pc.toml` configures (the engine's, the data
//! refresh's, the runner's): `{name}` placeholders are replaced with values
//! the guard knows (`{bundle}`, `{site}`, …). A typo never runs: an unknown
//! placeholder or an unclosed brace refuses the whole command.

/// Every argument with its placeholders replaced.
pub fn expand(args: &[String], vars: &[(&str, String)]) -> Result<Vec<String>, String> {
    args.iter().map(|a| expand_one(a, vars)).collect()
}

fn expand_one(arg: &str, vars: &[(&str, String)]) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        let (before, from_brace) = rest.split_at(open);
        out.push_str(before);
        let inside = from_brace.get(1..).unwrap_or_default();
        let close = inside
            .find('}')
            .ok_or_else(|| format!("unclosed placeholder in {arg:?}"))?;
        let (name, from_close) = inside.split_at(close);
        let value = vars
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| format!("unknown placeholder {{{name}}} in {arg:?}"))?;
        out.push_str(value);
        rest = from_close.get(1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vec<(&'static str, String)> {
        vec![
            ("bundle", "C:\\IEM\\bundles\\abc".into()),
            ("site", "C:\\IEM\\site.toml".into()),
        ]
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn placeholders_are_replaced_anywhere_in_an_argument() {
        assert_eq!(
            expand(
                &args(&[
                    "{bundle}\\iem-migrate.exe",
                    "band",
                    "--site={site}",
                    "{site}{bundle}"
                ]),
                &vars()
            )
            .unwrap(),
            [
                "C:\\IEM\\bundles\\abc\\iem-migrate.exe",
                "band",
                "--site=C:\\IEM\\site.toml",
                "C:\\IEM\\site.tomlC:\\IEM\\bundles\\abc",
            ]
        );
        assert_eq!(expand(&[], &vars()).unwrap(), Vec::<String>::new());
        assert_eq!(expand(&args(&["", "}"]), &vars()).unwrap(), ["", "}"]);
    }

    #[test]
    fn an_unknown_or_unclosed_placeholder_refuses_the_command() {
        assert_eq!(
            expand(&args(&["ok", "{sit}"]), &vars()).unwrap_err(),
            "unknown placeholder {sit} in \"{sit}\""
        );
        assert_eq!(
            expand(&args(&["{site"]), &vars()).unwrap_err(),
            "unclosed placeholder in \"{site\""
        );
        assert_eq!(
            expand(&args(&["{}"]), &vars()).unwrap_err(),
            "unknown placeholder {} in \"{}\""
        );
        // Each name takes its own value.
        assert_eq!(
            expand(&args(&["{site}"]), &vars()).unwrap(),
            ["C:\\IEM\\site.toml"]
        );
    }
}
