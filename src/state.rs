use crate::types::{format_set, parse_set, ProviderId, ProviderSet, Result};
use std::fmt::Write as _;
use std::fs;

const STATE_MODE: u32 = 0o600;
/// Cap state-file reads so a corrupted/huge file can't be slurped whole.
const MAX_STATE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Desired {
    pub generation: u64,
    pub providers: ProviderSet,
}

impl Desired {
    pub fn serialize(&self) -> String {
        format!(
            "generation {}\nproviders{}\n",
            self.generation,
            providers_suffix(&self.providers)
        )
    }

    pub fn parse(text: &str) -> Result<Desired> {
        let mut generation = 0u64;
        let mut providers = ProviderSet::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("generation ") {
                generation = rest.trim().parse()?;
            } else if let Some(rest) = line.strip_prefix("providers ") {
                providers = parse_set(rest.trim())?;
            } else if line == "providers" {
                providers = ProviderSet::new();
            }
        }
        Ok(Desired {
            generation,
            providers,
        })
    }
}

/// Render a set as a leading-space-prefixed suffix, or "" for the empty set, so
/// serialization never emits a trailing-space "providers " / "active " line.
fn providers_suffix(set: &ProviderSet) -> String {
    if set.is_empty() {
        String::new()
    } else {
        format!(" {}", format_set(set))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub generation: u64,
    pub active: ProviderSet,
    pub unavailable: Vec<(ProviderId, String)>,
}

impl Status {
    pub fn serialize(&self) -> String {
        let mut s = format!(
            "generation {}\nactive{}\n",
            self.generation,
            providers_suffix(&self.active)
        );
        for (id, reason) in &self.unavailable {
            let _ = writeln!(s, "unavailable {}:{}", id.as_str(), reason);
        }
        s
    }
}

/// Read a state file, capping the size. Absent => None.
fn read_capped(path: &str) -> Result<Option<String>> {
    match fs::metadata(path) {
        Ok(md) if md.len() > MAX_STATE_BYTES => {
            Err(format!("{path}: state file too large ({} bytes)", md.len()).into())
        }
        Ok(_) => Ok(Some(fs::read_to_string(path)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read desired state. Absent file => None (unset/idle). Present => Some.
pub fn read_desired(path: &str) -> Result<Option<Desired>> {
    match read_capped(path)? {
        Some(text) => Ok(Some(Desired::parse(&text)?)),
        None => Ok(None),
    }
}

pub fn write_desired(path: &str, d: &Desired) -> Result<()> {
    crate::fsutil::write_atomic(path, &d.serialize(), STATE_MODE)
}

pub fn write_status(path: &str, s: &Status) -> Result<()> {
    crate::fsutil::write_atomic(path, &s.serialize(), STATE_MODE)
}

pub fn read_status(path: &str) -> Result<Option<Status>> {
    match read_capped(path)? {
        Some(text) => {
            let mut generation = 0u64;
            let mut active = ProviderSet::new();
            let mut unavailable = Vec::new();
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("generation ") {
                    generation = rest.trim().parse()?;
                } else if let Some(rest) = line.trim().strip_prefix("unavailable ") {
                    if let Some((id, reason)) = rest.split_once(':') {
                        if let Ok(id) = id.parse::<ProviderId>() {
                            unavailable.push((id, reason.to_string()));
                        }
                    }
                } else if let Some(rest) = line.trim().strip_prefix("active") {
                    active = parse_set(rest.trim())?;
                }
            }
            Ok(Some(Status {
                generation,
                active,
                unavailable,
            }))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_roundtrip_with_providers() {
        let d = Desired {
            generation: 7,
            providers: parse_set("mullvad,tailscale").unwrap(),
        };
        let parsed = Desired::parse(&d.serialize()).unwrap();
        assert_eq!(parsed, d);
    }

    #[test]
    fn desired_empty_set_is_explicit_none() {
        let d = Desired {
            generation: 1,
            providers: ProviderSet::new(),
        };
        let text = d.serialize();
        assert_eq!(text, "generation 1\nproviders\n"); // no trailing space
        let parsed = Desired::parse(&text).unwrap();
        assert_eq!(parsed.generation, 1);
        assert!(parsed.providers.is_empty());
    }

    #[test]
    fn status_empty_active_has_no_trailing_space() {
        let s = Status {
            generation: 4,
            active: ProviderSet::new(),
            unavailable: Vec::new(),
        };
        assert_eq!(s.serialize(), "generation 4\nactive\n");
    }

    #[test]
    fn status_serialize_includes_unavailable() {
        let s = Status {
            generation: 2,
            active: parse_set("mullvad").unwrap(),
            unavailable: vec![(ProviderId::Tailscale, "not logged in".into())],
        };
        let out = s.serialize();
        assert!(out.contains("generation 2"));
        assert!(out.contains("active mullvad"));
        assert!(out.contains("unavailable tailscale:not logged in"));
    }

    fn temp_path(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("vpnmux-test-{}-{}", tag, std::process::id()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn read_absent_desired_is_none() {
        let path = temp_path("absent");
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_desired(&path).unwrap(), None);
    }

    #[test]
    fn write_then_read_desired() {
        let path = temp_path("rw");
        let d = Desired {
            generation: 5,
            providers: parse_set("tailscale").unwrap(),
        };
        write_desired(&path, &d).unwrap();
        assert_eq!(read_desired(&path).unwrap(), Some(d));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn write_then_read_status_preserves_unavailable() {
        let path = temp_path("status-unavail");
        let s = Status {
            generation: 3,
            active: parse_set("mullvad").unwrap(),
            unavailable: vec![(ProviderId::Tailscale, "not logged in".into())],
        };
        write_status(&path, &s).unwrap();
        let back = read_status(&path).unwrap().unwrap();
        assert_eq!(
            back.unavailable,
            vec![(ProviderId::Tailscale, "not logged in".to_string())]
        );
        std::fs::remove_file(&path).unwrap();
    }
}
