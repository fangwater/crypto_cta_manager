//! Source-specific theoretical pricing; independent of live order execution.
use crate::config::{AppConfig, SourceConfig};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModel {
    #[default]
    Twap5m,
    Vwap3m,
    Twap3m,
    InstantTaker,
}

impl ExecutionModel {
    pub fn for_source(source: &SourceConfig) -> Result<Self> {
        let values = crate::market_rules::source_values(source)?;
        Ok(
            match crate::market_rules::execution_backend(&source.venue, &values)? {
                "ltp" => Self::Vwap3m,
                _ => Self::Twap5m,
            },
        )
    }

    pub fn sample_count(self) -> usize {
        match self {
            Self::Twap5m => 5,
            Self::Vwap3m | Self::Twap3m => 3,
            Self::InstantTaker => 1,
        }
    }

    pub fn window_secs(self) -> u64 {
        if self == Self::InstantTaker {
            0
        } else {
            self.sample_count() as u64 * 60
        }
    }

    pub fn open_ts_us(self, received_at_us: i64) -> i64 {
        if self == Self::InstantTaker {
            received_at_us.div_euclid(crate::kline::MINUTE_US) * crate::kline::MINUTE_US
        } else {
            crate::kline::first_complete_open(received_at_us)
        }
    }

    pub fn missing_volume_reason(self, symbol: &str) -> String {
        match self {
            Self::Twap3m => format!("{symbol} 的三分钟窗口存在无成交分钟，理论3min_twap 不可用"),
            _ => format!("{symbol} 的三分钟窗口总成交量为零，理论3min_vwap 不可用"),
        }
    }

    pub fn price_basis(self) -> &'static str {
        match self {
            Self::Twap5m => {
                "equal_qty_over_traded_minutes_in_complete_5m_window_or_all_empty_close"
            }
            Self::Vwap3m => "total_quote_over_total_base_in_complete_3m_window_or_all_empty_close",
            Self::Twap3m => "arithmetic_mean_of_3_complete_minute_vwaps_or_empty_minute_close",
            Self::InstantTaker => "arrival_minute_open_as_instant_taker_estimate",
        }
    }

    pub fn nav_price_basis(self) -> &'static str {
        match self {
            Self::Twap5m => {
                "equal_qty_over_traded_minutes_in_complete_5m_window_or_all_empty_close+closed_1m_close_mark"
            }
            Self::Vwap3m => {
                "total_quote_over_total_base_in_complete_3m_window_or_all_empty_close+closed_1m_close_mark"
            }
            Self::Twap3m => {
                "arithmetic_mean_of_3_complete_minute_vwaps_or_empty_minute_close+closed_1m_close_mark"
            }
            Self::InstantTaker => {
                "arrival_minute_open_as_instant_taker_estimate+closed_1m_close_mark"
            }
        }
    }
}

pub type ExecutionModels = BTreeMap<String, ExecutionModel>;

/// Native sources keep their existing model in every portfolio comparison.
pub fn select_reference(models: &mut ExecutionModels, reference: ExecutionModel) {
    for model in models.values_mut() {
        if *model == ExecutionModel::Vwap3m {
            *model = reference;
        }
    }
}

pub fn references(config: &AppConfig, sources: &[String]) -> Result<Vec<ExecutionModel>> {
    let models = resolve_models(config, sources)?;
    Ok(
        if models
            .values()
            .any(|model| *model == ExecutionModel::Vwap3m)
        {
            vec![
                ExecutionModel::Vwap3m,
                ExecutionModel::Twap3m,
                ExecutionModel::InstantTaker,
            ]
        } else {
            vec![ExecutionModel::Twap5m]
        },
    )
}

pub fn resolve_models(config: &AppConfig, sources: &[String]) -> Result<ExecutionModels> {
    config
        .sources
        .iter()
        .filter(|source| source.enabled && (sources.is_empty() || sources.contains(&source.id)))
        .map(|source| {
            Ok((
                source.id.clone(),
                ExecutionModel::for_source(source).with_context(|| {
                    format!("resolve theoretical execution model for {}", source.id)
                })?,
            ))
        })
        .collect()
}

pub fn scope_model(models: &ExecutionModels) -> Option<ExecutionModel> {
    let first = models.values().next().copied().unwrap_or_default();
    models
        .values()
        .all(|model| *model == first)
        .then_some(first)
}

pub fn scope_window_secs(models: &ExecutionModels) -> u64 {
    models
        .values()
        .map(|model| model.window_secs())
        .max()
        .unwrap_or(300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_backend_per_exchange_without_inferring_account_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.env");
        let mut config: AppConfig =
            toml::from_str(include_str!("../config/cta-manager.example.toml")).unwrap();
        let source = &mut config.sources[0];
        source.id = "opaque-source".into();
        source.account = "ordinary-label".into();
        source.env_path = Some(path.clone());
        for backend in ["rapidx", "ltp", "liquidity", "liquiditytech"] {
            std::fs::write(
                &path,
                format!("TRADE_ENGINE_EXEC_BACKEND_MAP='binance={backend}'\n"),
            )
            .unwrap();
            assert_eq!(
                ExecutionModel::for_source(source).unwrap(),
                ExecutionModel::Vwap3m
            );
        }
        source.account = "lgq_RX01_rapidx".into();
        std::fs::write(&path, "TRADE_ENGINE_EXEC_BACKEND='rapidx'\nTRADE_ENGINE_EXEC_BACKEND_MAP='binance=native,okex=rapidx'\n").unwrap();
        assert_eq!(
            ExecutionModel::for_source(source).unwrap(),
            ExecutionModel::Twap5m
        );
        source.venue = "okex-futures".into();
        assert_eq!(
            ExecutionModel::for_source(source).unwrap(),
            ExecutionModel::Vwap3m
        );
        std::fs::write(&path, "TRADE_ENGINE_EXEC_BACKEND='invalid'\n").unwrap();
        assert!(ExecutionModel::for_source(source).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(ExecutionModel::for_source(source).is_err());
        assert_eq!(
            serde_json::to_string(&ExecutionModel::Vwap3m).unwrap(),
            "\"vwap3m\""
        );
    }
}
