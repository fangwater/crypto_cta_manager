//! Explicit, token-authenticated export of this host's cached account facts.
//! No recursive external aggregation, order events, credentials or write routes.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub token_env: String,
    pub source_ids: Vec<String>,
}
impl Settings {
    pub fn validate(&self, sources: &[SourceConfig]) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            !self.token_env.is_empty()
                && self
                    .token_env
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid overview export token environment"
        );
        ensure!(
            !self.source_ids.is_empty(),
            "overview export requires explicit source identities"
        );
        ensure!(
            self.source_ids
                .iter()
                .all(|id| sources.iter().any(|s| s.enabled && s.id == *id)),
            "overview export source is not enabled"
        );
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct OverviewExport {
    token_hash: Option<[u8; 32]>,
    sources: Vec<SourceConfig>,
}
impl OverviewExport {
    pub fn allows_source(&self, id: &str) -> bool {
        self.sources.iter().any(|source| source.id == id)
    }
    pub fn new(settings: &Settings, sources: &[SourceConfig]) -> Result<Self> {
        settings.validate(sources)?;
        if !settings.enabled {
            return Ok(Self::default());
        }
        let token = std::env::var(&settings.token_env).context("overview export token missing")?;
        ensure!(
            (32..=256).contains(&token.len()),
            "overview export token must have 32-256 characters"
        );
        Ok(Self {
            token_hash: Some(Sha256::digest(token.as_bytes()).into()),
            sources: sources
                .iter()
                .filter(|s| settings.source_ids.contains(&s.id))
                .cloned()
                .collect(),
        })
    }
    pub fn authorized(&self, authorization: Option<&str>) -> bool {
        let Some(expected) = self.token_hash else {
            return false;
        };
        let Some(token) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
            return false;
        };
        expected
            .iter()
            .zip(Sha256::digest(token.as_bytes()))
            .fold(0u8, |v, (a, b)| v | (a ^ b))
            == 0
    }
    pub fn report(
        &self,
        overview: &AccountOverview,
        selected: Option<i64>,
        alerts: &OverviewAlertSettings,
    ) -> Result<OverviewResponse> {
        let mut report = overview.report(&self.sources, selected, alerts)?;
        let time = now_ms();
        for row in &mut report.accounts {
            row.collection_status = Some(
                if selected.is_some_and(|day| day < day_start(time)) {
                    if row.equity.is_some() {
                        "historical"
                    } else {
                        "unavailable"
                    }
                } else if row
                    .date_ms
                    .is_some_and(|ts| (0..=90_000).contains(&(time - ts)))
                {
                    "live"
                } else if row.equity.is_some() {
                    "stale"
                } else {
                    "unavailable"
                }
                .into(),
            );
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_requires_separate_token_and_explicit_enabled_sources() {
        let export = OverviewExport {
            token_hash: Some(Sha256::digest(b"independent-test-token").into()),
            sources: vec![],
        };
        assert!(!export.authorized(None));
        assert!(!export.authorized(Some("cta_session=independent-test-token")));
        assert!(!export.authorized(Some("Bearer wrong")));
        assert!(export.authorized(Some("Bearer independent-test-token")));
        assert!(!OverviewExport::default().authorized(Some("Bearer independent-test-token")));
        assert!(
            Settings {
                enabled: true,
                token_env: "TEST_TOKEN".into(),
                source_ids: vec![]
            }
            .validate(&[])
            .is_err()
        );
        assert!(
            Settings {
                enabled: true,
                token_env: "TEST_TOKEN".into(),
                source_ids: vec!["unknown".into()]
            }
            .validate(&[])
            .is_err()
        );
    }
}
