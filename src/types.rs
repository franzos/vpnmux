use crate::mullvad::Mullvad;
use crate::provider::Provider;
use crate::tailscale::Tailscale;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderId {
    Mullvad,
    Tailscale,
}

impl ProviderId {
    /// Every provider vpnmux knows about. Add new providers here.
    pub const ALL: [ProviderId; 2] = [ProviderId::Mullvad, ProviderId::Tailscale];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Mullvad => "mullvad",
            ProviderId::Tailscale => "tailscale",
        }
    }

    /// Build the concrete provider for this id, wired with resolved bin paths.
    pub fn provider(self, mullvad_bin: &str, tailscale_bin: &str, nft: &str) -> Box<dyn Provider> {
        match self {
            ProviderId::Mullvad => Box::new(Mullvad {
                bin: mullvad_bin.to_string(),
                nft: nft.to_string(),
            }),
            ProviderId::Tailscale => Box::new(Tailscale {
                bin: tailscale_bin.to_string(),
            }),
        }
    }
}

/// The single provider registry: every known provider, built from `ALL`.
/// Used by both `cli::set` and `daemon::run` so the list lives in one place.
pub fn providers(mullvad_bin: &str, tailscale_bin: &str, nft: &str) -> Vec<Box<dyn Provider>> {
    ProviderId::ALL
        .iter()
        .map(|id| id.provider(mullvad_bin, tailscale_bin, nft))
        .collect()
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderId {
    type Err = Box<dyn std::error::Error>;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim() {
            "mullvad" => Ok(ProviderId::Mullvad),
            "tailscale" => Ok(ProviderId::Tailscale),
            other => Err(format!("unknown provider: {other:?}").into()),
        }
    }
}

pub type ProviderSet = BTreeSet<ProviderId>;

/// Parse a comma-separated id list ("mullvad,tailscale"); empty string = empty set.
pub fn parse_set(s: &str) -> Result<ProviderSet> {
    let mut set = ProviderSet::new();
    for part in s.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        set.insert(p.parse()?);
    }
    Ok(set)
}

/// Render a set as a sorted comma list ("mullvad,tailscale"); empty set = "".
pub fn format_set(set: &ProviderSet) -> String {
    set.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_roundtrip() {
        let set = parse_set("tailscale,mullvad").unwrap();
        assert!(set.contains(&ProviderId::Mullvad));
        assert!(set.contains(&ProviderId::Tailscale));
        assert_eq!(format_set(&set), "mullvad,tailscale"); // sorted
    }

    #[test]
    fn empty_string_is_empty_set() {
        assert!(parse_set("").unwrap().is_empty());
        assert_eq!(format_set(&ProviderSet::new()), "");
    }

    #[test]
    fn unknown_provider_errors() {
        assert!(parse_set("mullvad,wireguard").is_err());
    }
}
