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
    /// Set one or more search filters (`key=value` pairs). An empty value clears
    /// that key. Applied on top of the active filters, then the last query re-runs.
    Filter(Vec<(String, String)>),
    /// Clear all active search filters.
    ClearFilters,
    /// Clear the active search (results + query), back to the recents list.
    ClearSearch,
}

/// The filter keys the TUI accepts, mirroring the CLI/MCP allowlist — shown in
/// usage errors. (`site-group` and `site_group` are both accepted.)
const FILTER_KEYS_HELP: &str = "status site region site-group location tenant role tag vrf";

/// Whether `key` is an accepted filter key (case-insensitive). The input layer is
/// an allowlist too, so the TUI can never send NetBox an unknown query param.
fn is_filter_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "status"
            | "site"
            | "region"
            | "site-group"
            | "site_group"
            | "location"
            | "tenant"
            | "role"
            | "tag"
            | "vrf"
    )
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
        "rack" => lookup(ObjectKind::Rack, "rack <name|id>"),
        "vrf" => lookup(ObjectKind::Vrf, "vrf <name|rd|id>"),
        "route-target" | "rt" => lookup(ObjectKind::RouteTarget, "route-target <name|id>"),
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
        // `filter` alone clears; `filter k=v …` sets one or more (allowlisted keys).
        "filter" => {
            if rest.is_empty() {
                return Ok(PaletteCommand::ClearFilters);
            }
            let mut pairs = Vec::new();
            for tok in rest.split_whitespace() {
                let (k, v) = tok.split_once('=').ok_or_else(|| {
                    format!(
                        "usage: filter key=value … (keys: {FILTER_KEYS_HELP}); 'filter' clears all"
                    )
                })?;
                if !is_filter_key(k) {
                    return Err(format!(
                        "unknown filter key '{k}'; keys: {FILTER_KEYS_HELP}"
                    ));
                }
                pairs.push((k.to_string(), v.to_string()));
            }
            Ok(PaletteCommand::Filter(pairs))
        }
        // `unfilter k …` clears one or more keys.
        "unfilter" => {
            if rest.is_empty() {
                return Err(format!("usage: unfilter <key> (keys: {FILTER_KEYS_HELP})"));
            }
            let mut pairs = Vec::new();
            for k in rest.split_whitespace() {
                if !is_filter_key(k) {
                    return Err(format!(
                        "unknown filter key '{k}'; keys: {FILTER_KEYS_HELP}"
                    ));
                }
                pairs.push((k.to_string(), String::new()));
            }
            Ok(PaletteCommand::Filter(pairs))
        }
        "clear-filters" => Ok(PaletteCommand::ClearFilters),
        "clear-search" | "clear" => Ok(PaletteCommand::ClearSearch),
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
        assert_eq!(
            parse("rack R1-42").unwrap(),
            PaletteCommand::Lookup {
                kind: ObjectKind::Rack,
                value: "R1-42".into()
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

    #[test]
    fn parses_filter_set_clear_and_unfilter() {
        assert_eq!(
            parse("filter status=active site=dc1").unwrap(),
            PaletteCommand::Filter(vec![
                ("status".into(), "active".into()),
                ("site".into(), "dc1".into()),
            ])
        );
        // bare `filter` (and `clear-filters`) clear everything.
        assert_eq!(parse("filter").unwrap(), PaletteCommand::ClearFilters);
        assert_eq!(
            parse("clear-filters").unwrap(),
            PaletteCommand::ClearFilters
        );
        // `unfilter k …` clears specific keys (empty value).
        assert_eq!(
            parse("unfilter status vrf").unwrap(),
            PaletteCommand::Filter(vec![
                ("status".into(), String::new()),
                ("vrf".into(), String::new()),
            ])
        );
    }

    #[test]
    fn filter_rejects_unknown_keys_and_bad_syntax() {
        assert!(parse("filter bogus=1").is_err(), "unknown key");
        assert!(parse("filter status").is_err(), "missing '='");
        assert!(parse("unfilter bogus").is_err(), "unknown key");
    }

    #[test]
    fn parses_clear_search() {
        assert_eq!(parse("clear-search").unwrap(), PaletteCommand::ClearSearch);
        assert_eq!(parse("clear").unwrap(), PaletteCommand::ClearSearch);
    }
}
