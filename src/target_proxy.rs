//! Account-scoped Binance USDT target projection into native Bybit contracts.
//! Contract aliases are explicit: digit prefixes alone do not identify an asset.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::market_rules::MarketRulesSnapshot;
use crate::order_config::TargetPosition;

const MAX_RULE_AGE_US: i64 = 180_000_000;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetProxyConfig {
    #[serde(default)]
    pub symbol_mappings: BTreeMap<String, ContractMapping>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractMapping {
    pub symbol: String,
    /// Units of the same underlying asset represented by one incoming qty.
    pub source_contract_multiplier: f64,
    /// Units of that underlying asset represented by one destination qty.
    pub target_contract_multiplier: f64,
}

impl TargetProxyConfig {
    pub fn validate(&self, venue: &str) -> Result<()> {
        ensure!(
            venue == "bybit-futures",
            "target_proxy requires a Bybit USDT perpetual source"
        );
        let mut destinations = std::collections::BTreeSet::new();
        for (input, mapping) in &self.symbol_mappings {
            for symbol in [input, &mapping.symbol] {
                ensure!(
                    is_usdt_contract(symbol),
                    "target_proxy mapping requires a normalized USDT contract: {symbol}"
                );
            }
            for multiplier in [
                mapping.source_contract_multiplier,
                mapping.target_contract_multiplier,
            ] {
                ensure!(
                    multiplier.is_finite() && multiplier > 0.0,
                    "target_proxy contract multipliers must be finite and positive"
                );
            }
            let ratio = mapping.source_contract_multiplier / mapping.target_contract_multiplier;
            ensure!(
                ratio.is_finite() && ratio > 0.0,
                "target_proxy contract multiplier ratio is out of range"
            );
            ensure!(
                destinations.insert(&mapping.symbol),
                "target_proxy has duplicate destination: {}",
                mapping.symbol
            );
        }
        Ok(())
    }
}

fn is_usdt_contract(symbol: &str) -> bool {
    symbol.len() > 4
        && symbol.ends_with("USDT")
        && symbol.to_uppercase() == symbol
        && crate::order_config::validate_exec_symbol(symbol).is_ok()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchedContract {
    pub symbol: String,
    pub source_contract_multiplier: f64,
    pub target_contract_multiplier: f64,
    pub quantity_multiplier: f64,
}

/// Frozen alongside each account in the original position-update archive.
/// Targets already include shares; reconstructors must not multiply them again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetProjection {
    pub rules_fetched_at_us: i64,
    pub targets: BTreeMap<String, TargetPosition>,
    pub matched: BTreeMap<String, MatchedContract>,
    pub filtered: BTreeMap<String, String>,
}

impl TargetProjection {
    pub fn ensure_fresh(&self, now_us: i64) -> Result<()> {
        validate_cache_time(self.rules_fetched_at_us, now_us)
    }

    pub fn remap<T: Clone>(&self, values: &BTreeMap<String, T>) -> BTreeMap<String, T> {
        self.matched
            .iter()
            .filter_map(|(input, mapping)| {
                values
                    .get(input)
                    .map(|value| (mapping.symbol.clone(), value.clone()))
            })
            .collect()
    }
}

fn validate_cache_time(fetched_at_us: i64, now_us: i64) -> Result<()> {
    let age = now_us
        .checked_sub(fetched_at_us)
        .context("invalid target_proxy cache timestamp")?;
    ensure!(
        fetched_at_us > 0 && (0..=MAX_RULE_AGE_US).contains(&age),
        "Bybit target_proxy market-rules cache is stale or from the future; retry after refresh"
    );
    Ok(())
}

pub fn project(
    config: &TargetProxyConfig,
    snapshot: &MarketRulesSnapshot,
    targets: &BTreeMap<String, TargetPosition>,
    shares: f64,
    now_us: i64,
) -> Result<TargetProjection> {
    config.validate(&snapshot.venue)?;
    snapshot.validate()?;
    ensure!(
        snapshot.execution_backend == "native" && snapshot.portfolio_id.is_none(),
        "target_proxy requires native Bybit rules"
    );
    validate_cache_time(snapshot.fetched_at_us, now_us)?;
    crate::strategy_catalog::validate_targets(targets).map_err(anyhow::Error::msg)?;
    crate::strategy_catalog::validate_nonnegative_multiplier(shares, "shares")
        .map_err(anyhow::Error::msg)?;
    let mut result = TargetProjection {
        rules_fetched_at_us: snapshot.fetched_at_us,
        targets: BTreeMap::new(),
        matched: BTreeMap::new(),
        filtered: BTreeMap::new(),
    };
    for (input, target) in targets {
        if !is_usdt_contract(input) {
            result
                .filtered
                .insert(input.clone(), "not a USDT perpetual target".into());
            continue;
        }
        let mapping = config.symbol_mappings.get(input);
        let symbol = mapping.map_or(input, |m| &m.symbol);
        let Some(rule) = snapshot.symbols.get(symbol) else {
            result
                .filtered
                .insert(input.clone(), "symbol absent from Bybit cache".into());
            continue;
        };
        if rule.status != "Trading" || rule.quote_asset != "USDT" {
            result.filtered.insert(
                input.clone(),
                format!("Bybit contract is not tradable: {}", rule.status),
            );
            continue;
        }
        let source_multiplier = mapping.map_or(1.0, |m| m.source_contract_multiplier);
        let target_multiplier = mapping.map_or(1.0, |m| m.target_contract_multiplier);
        let ratio = source_multiplier / target_multiplier;
        let qty = if shares == 0.0 || target.clear {
            0.0
        } else {
            target.qty * ratio * shares
        };
        ensure!(qty.is_finite(), "target_proxy quantity overflow: {input}");
        ensure!(
            target.qty == 0.0 || shares == 0.0 || qty != 0.0,
            "target_proxy quantity underflow: {input}"
        );
        ensure!(
            result
                .targets
                .insert(symbol.clone(), TargetPosition { qty, ..*target })
                .is_none(),
            "target_proxy inputs collide at destination {symbol}; publish a single unambiguous target"
        );
        result.matched.insert(
            input.clone(),
            MatchedContract {
                symbol: symbol.clone(),
                source_contract_multiplier: source_multiplier,
                target_contract_multiplier: target_multiplier,
                quantity_multiplier: ratio,
            },
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_rules::MarketRule;

    fn snapshot() -> MarketRulesSnapshot {
        MarketRulesSnapshot {
            venue: "bybit-futures".into(),
            execution_backend: "native".into(),
            portfolio_id: None,
            fetched_at_us: 1_000_000,
            symbols: [
                "BTCUSDT",
                "PEPEUSDT",
                "1000PEPEUSDT",
                "1INCHUSDT",
                "HALTUSDT",
            ]
            .into_iter()
            .map(|symbol| {
                (
                    symbol.into(),
                    MarketRule {
                        status: if symbol == "HALTUSDT" {
                            "Closed"
                        } else {
                            "Trading"
                        }
                        .into(),
                        base_asset: symbol.strip_suffix("USDT").unwrap().into(),
                        quote_asset: "USDT".into(),
                        price_tick: "0.001".into(),
                        qty_step: "0.001".into(),
                        min_qty: "0.001".into(),
                        min_notional: None,
                        contract_multiplier: None,
                    },
                )
            })
            .collect(),
        }
    }

    fn targets(value: serde_json::Value) -> BTreeMap<String, TargetPosition> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn filters_missing_inactive_and_other_markets_without_guessing_names() {
        let input = targets(
            serde_json::json!({"BTCUSDT":{"qty":-2,"signal":-1},"MISSINGUSDT":7,"HALTUSDT":2,"BTC":3,"BTCUSDC":4,"1INCHUSDT":5,"10000PEPEUSDT":9}),
        );
        let result = project(
            &TargetProxyConfig::default(),
            &snapshot(),
            &input,
            3.0,
            2_000_000,
        )
        .unwrap();
        assert_eq!(result.targets.len(), 2);
        assert_eq!(result.filtered.len(), 5);
        assert_eq!(result.targets["BTCUSDT"].qty, -6.0);
        assert_eq!(result.targets["BTCUSDT"].signal, -1);
        assert_eq!(result.targets["1INCHUSDT"].qty, 15.0);
        assert_eq!(input["BTCUSDT"].qty, -2.0);
        let empty = targets(serde_json::json!({"MISSINGUSDT":7}));
        assert!(
            project(
                &TargetProxyConfig::default(),
                &snapshot(),
                &empty,
                1.0,
                2_000_000
            )
            .unwrap()
            .targets
            .is_empty()
        );
    }

    #[test]
    fn converts_units_both_directions_and_preserves_clear_signals_and_overrides() {
        for (input, output, from, to, expected) in [
            ("1000PEPEUSDT", "PEPEUSDT", 1000.0, 1.0, -6000.0),
            ("PEPEUSDT", "1000PEPEUSDT", 1.0, 1000.0, -0.006),
        ] {
            let config = TargetProxyConfig {
                symbol_mappings: BTreeMap::from([(
                    input.into(),
                    ContractMapping {
                        symbol: output.into(),
                        source_contract_multiplier: from,
                        target_contract_multiplier: to,
                    },
                )]),
            };
            let raw = targets(serde_json::json!({input:{"qty":-2,"signal":2}}));
            let result = project(&config, &snapshot(), &raw, 3.0, 2_000_000).unwrap();
            assert_eq!(result.targets[output].qty, expected);
            assert_eq!(result.targets[output].signal, 2);
            assert_eq!(
                result.remap(&BTreeMap::from([(input.into(), "custom")])),
                BTreeMap::from([(output.into(), "custom")])
            );
            let clear = targets(serde_json::json!({input:{"qty":"clear","signal":-2}}));
            assert!(
                project(&config, &snapshot(), &clear, 3.0, 2_000_000)
                    .unwrap()
                    .targets[output]
                    .clear
            );
            let stopped = project(&config, &snapshot(), &raw, 0.0, 2_000_000).unwrap();
            assert_eq!(stopped.targets[output].qty, 0.0);
            assert!(!stopped.targets[output].clear);
        }
    }

    #[test]
    fn rejects_stale_provenance_collisions_and_invalid_arithmetic() {
        let input = targets(serde_json::json!({"BTCUSDT":1}));
        let config = TargetProxyConfig::default();
        for now in [999_999, 181_000_001] {
            assert!(project(&config, &snapshot(), &input, 1.0, now).is_err());
        }
        let mut bad = snapshot();
        bad.execution_backend = "ltp".into();
        assert!(project(&config, &bad, &input, 1.0, 2_000_000).is_err());
        assert!(project(&config, &snapshot(), &input, f64::INFINITY, 2_000_000).is_err());
        let overflow = targets(serde_json::json!({"BTCUSDT":f64::MAX}));
        assert!(project(&config, &snapshot(), &overflow, 2.0, 2_000_000).is_err());
        let mut config = TargetProxyConfig {
            symbol_mappings: BTreeMap::from([(
                "PEPEUSDT".into(),
                ContractMapping {
                    symbol: "1000PEPEUSDT".into(),
                    source_contract_multiplier: 1.0,
                    target_contract_multiplier: 1000.0,
                },
            )]),
        };
        let collision = targets(serde_json::json!({"PEPEUSDT":2,"1000PEPEUSDT":3}));
        assert!(project(&config, &snapshot(), &collision, 1.0, 2_000_000).is_err());
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            config
                .symbol_mappings
                .get_mut("PEPEUSDT")
                .unwrap()
                .source_contract_multiplier = invalid;
            assert!(config.validate("bybit-futures").is_err());
        }
        assert!(
            TargetProxyConfig::default()
                .validate("binance-futures")
                .is_err()
        );
    }
}
