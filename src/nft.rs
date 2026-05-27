use crate::runner::Runner;
use crate::types::Result;

const RULES: [(&str, &str); 2] = [("input", "iifname"), ("output", "oifname")];

/// Ensure both tailscale* accept rules exist in `inet mullvad` (check-then-insert).
/// No-op when the table is absent (Mullvad not connected).
pub fn ensure_whitelist(nft: &str, r: &dyn Runner) -> Result<()> {
    let listing = r.run(nft, &["list", "table", "inet", "mullvad"])?;
    if !listing.ok() {
        return Ok(()); // table absent → nothing to whitelist
    }
    for (chain, dir) in RULES {
        let needle = format!("{dir} \"tailscale*\"");
        if !listing.stdout.contains(&needle) {
            crate::info!(
                "nft: inserted tailscale whitelist [{} {}] into inet mullvad (table was rebuilt or rule missing)",
                chain,
                dir
            );
            r.run(
                nft,
                &[
                    "insert",
                    "rule",
                    "inet",
                    "mullvad",
                    chain,
                    dir,
                    "tailscale*",
                    "accept",
                ],
            )?;
        }
    }
    Ok(())
}

/// Delete the `inet mullvad` table (lockdown-off Mullvad teardown). Returns
/// whether a table was actually removed, so we only log a real teardown rather
/// than announcing one every tick when the table is already gone.
pub fn delete_table(nft: &str, r: &dyn Runner) -> Result<bool> {
    let removed = r.run(nft, &["delete", "table", "inet", "mullvad"])?.ok();
    if removed {
        crate::info!("nft: removed table inet mullvad (mullvad teardown)");
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    #[test]
    fn inserts_both_rules_when_missing() {
        let r = MockRunner::new().on(
            "nft list table inet mullvad",
            0,
            "table inet mullvad {\n chain input {\n }\n}",
        );
        ensure_whitelist("nft", &r).unwrap();
        assert!(r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
        assert!(r.called("nft insert rule inet mullvad output oifname tailscale* accept"));
    }

    #[test]
    fn skips_when_rules_present() {
        let listing = "chain input {\n iifname \"tailscale*\" accept\n }\n \
                       chain output {\n oifname \"tailscale*\" accept\n }";
        let r = MockRunner::new().on("nft list table inet mullvad", 0, listing);
        ensure_whitelist("nft", &r).unwrap();
        assert!(!r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
    }

    #[test]
    fn noop_when_table_absent() {
        let r = MockRunner::new().on("nft list table inet mullvad", 1, "");
        ensure_whitelist("nft", &r).unwrap();
        assert!(!r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
    }

    #[test]
    fn delete_table_reports_real_removal() {
        let r = MockRunner::new().on("nft delete table inet mullvad", 0, "");
        assert!(delete_table("nft", &r).unwrap());
        assert!(r.called("nft delete table inet mullvad"));
    }

    #[test]
    fn delete_table_is_noop_when_absent() {
        let r = MockRunner::new().on(
            "nft delete table inet mullvad",
            1,
            "Error: No such file or directory",
        );
        assert!(!delete_table("nft", &r).unwrap());
    }
}
