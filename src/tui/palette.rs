//! Command-palette parsing (`:` mode).

use crate::netbox::search::ObjectKind;

/// A parsed palette command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteCommand {
    /// Resolve an object by reference and open its detail.
    Lookup { kind: ObjectKind, value: String },
    /// Run a server search.
    Search(String),
    /// Open the selected result in a browser.
    Open,
    /// Copy the selected value.
    Copy,
    /// Switch theme by name.
    Theme(String),
    /// Switch the active NetBox profile by name.
    Profile(String),
    /// Open the in-app Config modal (Profiles section).
    Config,
    /// Re-run the last search.
    Refresh,
}

/// Parse palette input. Unknown verbs are treated as a bare search query.
pub fn parse(input: &str) -> Result<PaletteCommand, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty command".into());
    }

    let (verb, rest) = match input.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (input, ""),
    };

    let lookup = |kind, usage: &str| {
        if rest.is_empty() {
            Err(format!("usage: {usage}"))
        } else {
            Ok(PaletteCommand::Lookup {
                kind,
                value: rest.to_string(),
            })
        }
    };

    match verb.to_lowercase().as_str() {
        "device" | "dev" => lookup(ObjectKind::Device, "device <name|id>"),
        "ip" => lookup(ObjectKind::IpAddress, "ip <address>"),
        "prefix" => lookup(ObjectKind::Prefix, "prefix <cidr>"),
        "vlan" => lookup(ObjectKind::Vlan, "vlan <vid|name>"),
        "site" => lookup(ObjectKind::Site, "site <name|slug>"),
        "find" | "search" => {
            if rest.is_empty() {
                Err("usage: find <query>".into())
            } else {
                Ok(PaletteCommand::Search(rest.to_string()))
            }
        }
        "open" | "o" => Ok(PaletteCommand::Open),
        "copy" | "y" => Ok(PaletteCommand::Copy),
        "theme" => {
            if rest.is_empty() {
                Err("usage: theme <name>".into())
            } else {
                Ok(PaletteCommand::Theme(rest.to_string()))
            }
        }
        "profile" | "prof" => {
            if rest.is_empty() {
                Err("usage: profile <name>".into())
            } else {
                Ok(PaletteCommand::Profile(rest.to_string()))
            }
        }
        "config" | "cfg" => Ok(PaletteCommand::Config),
        "refresh" | "r" => Ok(PaletteCommand::Refresh),
        // Anything else: treat the whole input as a search query.
        _ => Ok(PaletteCommand::Search(input.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lookup_verbs() {
        assert_eq!(
            parse("device edge01").unwrap(),
            PaletteCommand::Lookup {
                kind: ObjectKind::Device,
                value: "edge01".into()
            }
        );
        assert_eq!(
            parse("ip 10.44.208.55").unwrap(),
            PaletteCommand::Lookup {
                kind: ObjectKind::IpAddress,
                value: "10.44.208.55".into()
            }
        );
    }

    #[test]
    fn bare_text_is_a_search() {
        assert_eq!(
            parse("edge01").unwrap(),
            PaletteCommand::Search("edge01".into())
        );
    }

    #[test]
    fn verbs_and_actions() {
        assert_eq!(parse("open").unwrap(), PaletteCommand::Open);
        assert_eq!(parse("y").unwrap(), PaletteCommand::Copy);
        assert_eq!(parse("refresh").unwrap(), PaletteCommand::Refresh);
        assert_eq!(
            parse("theme nord").unwrap(),
            PaletteCommand::Theme("nord".into())
        );
    }

    #[test]
    fn parses_profile_verb_and_alias() {
        // `profile <name>` (and the `prof` alias) jump to a named profile. The
        // name is kept verbatim; resolution against the configured set happens in
        // the app handler, not the parser.
        assert_eq!(
            parse("profile lab").unwrap(),
            PaletteCommand::Profile("lab".into())
        );
        assert_eq!(
            parse("prof prod").unwrap(),
            PaletteCommand::Profile("prod".into())
        );
        // A name with surrounding whitespace is trimmed like the other verbs.
        assert_eq!(
            parse("profile   work  ").unwrap(),
            PaletteCommand::Profile("work".into())
        );
    }

    #[test]
    fn parses_config_verb_and_alias() {
        assert_eq!(parse("config").unwrap(), PaletteCommand::Config);
        assert_eq!(parse("cfg").unwrap(), PaletteCommand::Config);
        // Trailing args are ignored — the modal opens on the Profiles section.
        assert_eq!(parse("config profiles").unwrap(), PaletteCommand::Config);
    }

    #[test]
    fn missing_args_and_empty_are_errors() {
        assert!(parse("device").is_err());
        assert!(parse("theme").is_err());
        assert!(parse("profile").is_err());
        assert!(parse("   ").is_err());
    }
}
