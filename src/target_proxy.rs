//! Account-scoped target projection into native Bybit and OKX markets.
//! OKX unit prefixes require a matching Binance index constituent.
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
    /// Interpret verified Binance USD-M unit prefixes when publishing to OKX.
    #[serde(default)]
    pub auto_binance_unit_prefixes: bool,
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
        if self.auto_binance_unit_prefixes {
            ensure!(
                venue == "okex-futures",
                "automatic Binance unit prefixes require an OKX source"
            );
            ensure!(
                self.symbol_mappings.is_empty(),
                "automatic OKX unit prefixes use exchange metadata exclusively"
            );
            return Ok(());
        }
        ensure!(
            venue == "bybit-futures",
            "explicit target mappings require a Bybit USDT perpetual source"
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

/// Candidate generation only. Binance's index must corroborate the venue,
/// unscaled instrument and scale before a candidate can change a target.
pub(crate) fn okx_unit_prefix_candidate(symbol: &str) -> Option<(&str, u32)> {
    if !is_usdt_contract(symbol) {
        return None;
    }
    let base = symbol.strip_suffix("USDT")?;
    let asset = base.trim_start_matches(|c: char| c.is_ascii_digit());
    let prefix = base.strip_suffix(asset)?;
    let scale: u32 = prefix.parse().ok()?;
    (scale > 1 && !prefix.starts_with('0') && asset.starts_with(|c: char| c.is_ascii_uppercase()))
        .then_some((asset, scale))
}

pub(crate) struct OkxUnitPrefixes {
    pub fetched_at_us: i64,
    pub mappings: BTreeMap<String, ContractMapping>,
}

fn live_okx_swap(snapshot: &MarketRulesSnapshot, symbol: &str) -> bool {
    snapshot.symbols.get(symbol).is_some_and(|rule| {
        rule.status == "live"
            && rule.quote_asset == "USDT"
            && rule.contract_multiplier.is_some()
            && symbol.strip_suffix("USDT") == Some(rule.base_asset.as_str())
    })
}

pub(crate) fn okx_unit_prefix_destination(
    snapshot: &MarketRulesSnapshot,
    input: &str,
) -> Option<(String, u32)> {
    let (asset, scale) = okx_unit_prefix_candidate(input)?;
    let symbol = format!("{asset}USDT");
    (!snapshot.symbols.contains_key(input) && live_okx_swap(snapshot, &symbol))
        .then_some((symbol, scale))
}

pub(crate) fn confirm_okx_unit_prefix(
    input: &str,
    symbol: String,
    scale: u32,
    index: &serde_json::Value,
) -> Option<ContractMapping> {
    let asset = symbol.strip_suffix("USDT")?;
    let expected = format!("{asset}-USDT*{scale}");
    let matches = index.get("symbol")?.as_str()? == input
        && index.get("constituents")?.as_array()?.iter().any(|leg| {
            matches!(leg["exchange"].as_str(), Some("okex" | "okx"))
                && leg["symbol"].as_str() == Some(expected.as_str())
        });
    matches.then_some(ContractMapping {
        symbol,
        source_contract_multiplier: f64::from(scale),
        target_contract_multiplier: 1.0,
    })
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
        "target_proxy market-rules cache is stale or from the future; retry after refresh"
    );
    Ok(())
}

/// Freeze each exchange leg before archive and publication. Runtime targets
/// remain ordinary symbols within the destination's existing market scope.
pub fn project_cross_spread(
    source: &crate::config::SourceConfig,
    targets: &BTreeMap<String, TargetPosition>,
    shares: f64,
    now_us: i64,
) -> Result<TargetProjection> {
    crate::strategy_catalog::validate_targets(targets).map_err(anyhow::Error::msg)?;
    crate::strategy_catalog::validate_nonnegative_multiplier(shares, "shares")
        .map_err(anyhow::Error::msg)?;
    let mut result = TargetProjection {
        rules_fetched_at_us: now_us,
        targets: BTreeMap::new(),
        matched: BTreeMap::new(),
        filtered: BTreeMap::new(),
    };
    for (key, target) in targets {
        crate::exec_routing::validate_target_symbol(source, key)?;
        let (venue, symbol) = key
            .split_once(':')
            .context("cross spread requires qualified targets")?;
        if venue != source.venue {
            result
                .filtered
                .insert(key.clone(), "owned by the other exchange leg".into());
            continue;
        }
        let qty = if target.clear || shares == 0.0 {
            0.0
        } else {
            target.qty * shares
        };
        ensure!(qty.is_finite(), "cross spread target quantity overflow");
        ensure!(
            shares == 0.0 || target.qty == 0.0 || qty != 0.0,
            "cross spread target quantity underflow"
        );
        result
            .targets
            .insert(symbol.into(), TargetPosition { qty, ..*target });
        result.matched.insert(
            key.clone(),
            MatchedContract {
                symbol: symbol.into(),
                source_contract_multiplier: 1.0,
                target_contract_multiplier: 1.0,
                quantity_multiplier: 1.0,
            },
        );
    }
    Ok(result)
}

pub(crate) fn project(
    config: &TargetProxyConfig,
    snapshot: &MarketRulesSnapshot,
    prefixes: Option<&OkxUnitPrefixes>,
    targets: &BTreeMap<String, TargetPosition>,
    shares: f64,
    now_us: i64,
) -> Result<TargetProjection> {
    config.validate(&snapshot.venue)?;
    snapshot.validate()?;
    ensure!(
        snapshot.execution_backend == "native" && snapshot.portfolio_id.is_none(),
        "target_proxy requires native market rules"
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
    let bybit = snapshot.venue == "bybit-futures";
    for (input, target) in targets {
        let mut mapping = config.symbol_mappings.get(input);
        if config.auto_binance_unit_prefixes
            && !snapshot.symbols.contains_key(input)
            && okx_unit_prefix_candidate(input).is_some()
        {
            mapping = prefixes.and_then(|proof| proof.mappings.get(input));
            if shares > 0.0 && target.qty != 0.0 {
                let verified = prefixes.context("OKX unit-prefix cache is loading")?;
                validate_cache_time(verified.fetched_at_us, now_us)?;
                let alias = mapping.with_context(|| {
                    format!("Binance index did not confirm an OKX unit-prefix mapping for {input}")
                })?;
                ensure!(
                    live_okx_swap(snapshot, &alias.symbol),
                    "mapped OKX contract {} is unavailable for {input}",
                    alias.symbol
                );
                result.rules_fetched_at_us = result.rules_fetched_at_us.min(verified.fetched_at_us);
            }
        }
        let symbol = mapping.map_or(input, |m| &m.symbol);
        if bybit {
            if !is_usdt_contract(input) {
                result
                    .filtered
                    .insert(input.clone(), "not a USDT perpetual target".into());
                continue;
            }
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
        } else {
            crate::exec_routing::symbol_market(&snapshot.venue, symbol)?;
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

    #[test]
    fn cross_spread_freezes_each_venue_leg_and_zero_share_clear_independently() {
        let config: crate::config::AppConfig =
            toml::from_str(include_str!("../config/cta-manager.example.toml")).unwrap();
        let mut source = config.sources[0].clone();
        let targets = BTreeMap::from([
            (
                "okex-futures:ETHUSDT".into(),
                TargetPosition::new(2.0, 0).unwrap(),
            ),
            (
                "binance-futures:ETHUSDT".into(),
                TargetPosition::new(-2.0, 1).unwrap(),
            ),
        ]);
        for (venue, expected) in [("okex-futures", 6.0), ("binance-futures", -6.0)] {
            source.venue = venue.into();
            let projected = project_cross_spread(&source, &targets, 3.0, 100).unwrap();
            assert_eq!(projected.targets.len(), 1);
            assert_eq!(projected.targets["ETHUSDT"].qty, expected);
            assert_eq!(projected.filtered.len(), 1);
            let overrides = BTreeMap::from([
                ("okex-futures:ETHUSDT".into(), "okx-template"),
                ("binance-futures:ETHUSDT".into(), "bn-template"),
            ]);
            assert_eq!(
                projected.remap(&overrides)["ETHUSDT"],
                if expected > 0.0 {
                    "okx-template"
                } else {
                    "bn-template"
                }
            );
            assert_eq!(
                project_cross_spread(&source, &targets, 0.0, 100)
                    .unwrap()
                    .targets["ETHUSDT"]
                    .qty,
                0.0
            );
        }
        let mut incomplete = targets.clone();
        incomplete.remove("okex-futures:ETHUSDT");
        assert!(project_cross_spread(&source, &incomplete, 1.0, 100).is_err());
        let mut mixed = targets.clone();
        mixed.insert("BTCUSDT".into(), TargetPosition::new(1.0, 0).unwrap());
        assert!(project_cross_spread(&source, &mixed, 1.0, 100).is_err());
    }

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
            None,
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
                None,
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
                ..Default::default()
            };
            let raw = targets(serde_json::json!({input:{"qty":-2,"signal":2}}));
            let result = project(&config, &snapshot(), None, &raw, 3.0, 2_000_000).unwrap();
            assert_eq!(result.targets[output].qty, expected);
            assert_eq!(result.targets[output].signal, 2);
            assert_eq!(
                result.remap(&BTreeMap::from([(input.into(), "custom")])),
                BTreeMap::from([(output.into(), "custom")])
            );
            let clear = targets(serde_json::json!({input:{"qty":"clear","signal":-2}}));
            assert!(
                project(&config, &snapshot(), None, &clear, 3.0, 2_000_000)
                    .unwrap()
                    .targets[output]
                    .clear
            );
            let stopped = project(&config, &snapshot(), None, &raw, 0.0, 2_000_000).unwrap();
            assert_eq!(stopped.targets[output].qty, 0.0);
            assert!(!stopped.targets[output].clear);
        }
    }

    #[test]
    fn okx_alias_scales_to_base_units_and_preserves_existing_market_routing() {
        let mut rules = snapshot();
        rules.venue = "okex-futures".into();
        let mut pepe_rule = rules.symbols.remove("PEPEUSDT").unwrap();
        pepe_rule.status = "live".into();
        pepe_rule.contract_multiplier = Some("10000000".into());
        rules.symbols = BTreeMap::from([("PEPEUSDT".into(), pepe_rule)]);
        let config: TargetProxyConfig =
            toml::from_str("auto_binance_unit_prefixes = true").unwrap();
        assert!(config.validate("bybit-futures").is_err());
        let verified = serde_json::json!({
            "symbol": "1000PEPEUSDT",
            "constituents": [{"exchange": "okex", "symbol": "PEPE-USDT*1000"}]
        });
        let mut prefixes = OkxUnitPrefixes {
            fetched_at_us: 1_000_000,
            mappings: BTreeMap::from([(
                "1000PEPEUSDT".into(),
                confirm_okx_unit_prefix("1000PEPEUSDT", "PEPEUSDT".into(), 1000, &verified)
                    .unwrap(),
            )]),
        };
        let input = targets(serde_json::json!({
            "1000PEPEUSDT": {"qty": -2000, "signal": -1},
            "BTC": 2,
            "BTCUSDT": 3,
        }));
        let projection = project(&config, &rules, Some(&prefixes), &input, 1.5, 2_000_000).unwrap();
        assert_eq!(projection.targets["PEPEUSDT"].qty, -3_000_000.0);
        assert_eq!(projection.targets["PEPEUSDT"].signal, -1);
        assert_eq!(projection.targets["BTC"].qty, 3.0);
        assert_eq!(projection.targets["BTCUSDT"].qty, 4.5);
        assert!(!projection.targets.contains_key("1000PEPEUSDT"));
        assert_eq!(
            projection.remap(&BTreeMap::from([("1000PEPEUSDT".into(), "custom")])),
            BTreeMap::from([("PEPEUSDT".into(), "custom")])
        );
        let mut collision = input.clone();
        collision.insert("PEPEUSDT".into(), TargetPosition::new(1.0, 0).unwrap());
        assert!(project(&config, &rules, Some(&prefixes), &collision, 1.0, 2_000_000).is_err());

        rules.symbols.get_mut("PEPEUSDT").unwrap().status = "suspend".into();
        assert!(project(&config, &rules, Some(&prefixes), &input, 1.0, 2_000_000).is_err());
        rules.symbols.get_mut("PEPEUSDT").unwrap().status = "live".into();
        rules.fetched_at_us = 182_000_000;
        assert!(project(&config, &rules, Some(&prefixes), &input, 1.0, 182_000_000).is_err());
        let stopped = project(&config, &rules, Some(&prefixes), &input, 0.0, 182_000_000).unwrap();
        assert_eq!(stopped.targets["PEPEUSDT"].qty, 0.0);
        let clear = targets(serde_json::json!({"1000PEPEUSDT": "clear"}));
        let cleared = project(&config, &rules, Some(&prefixes), &clear, 1.0, 182_000_000).unwrap();
        assert!(cleared.targets["PEPEUSDT"].clear);
        prefixes.mappings.clear();
        assert!(project(&config, &rules, Some(&prefixes), &input, 1.0, 182_000_000).is_err());
    }

    #[test]
    fn okx_prefixes_require_index_evidence_and_prefer_the_exact_contract() {
        let mut rules = snapshot();
        rules.venue = "okex-futures".into();
        rules.symbols.retain(|symbol, _| symbol != "1000PEPEUSDT");
        for rule in rules.symbols.values_mut() {
            rule.status = "live".into();
            rule.contract_multiplier = Some("10000000".into());
        }
        let config = TargetProxyConfig {
            auto_binance_unit_prefixes: true,
            ..Default::default()
        };
        let mut prefixes = OkxUnitPrefixes {
            fetched_at_us: 950_000,
            mappings: BTreeMap::new(),
        };
        for (input, asset, units) in [
            ("1000PEPEUSDT", "PEPE", 1000),
            ("10000BONKUSDT", "BONK", 10000),
        ] {
            let destination = format!("{asset}USDT");
            let mut rule = rules.symbols["PEPEUSDT"].clone();
            rule.base_asset = asset.into();
            rules.symbols.insert(destination.clone(), rule);
            assert_eq!(
                okx_unit_prefix_destination(&rules, input),
                Some((destination.clone(), units))
            );
            let proof = serde_json::json!({
                "symbol": input,
                "constituents": [{"exchange": "okex", "symbol": format!("{asset}-USDT*{units}")}],
            });
            assert!(
                confirm_okx_unit_prefix(input, destination.clone(), units + 1, &proof).is_none()
            );
            prefixes.mappings.insert(
                input.into(),
                confirm_okx_unit_prefix(input, destination, units, &proof).unwrap(),
            );
        }
        assert!(okx_unit_prefix_candidate("1INCHUSDT").is_none());
        let input = targets(serde_json::json!({"1000PEPEUSDT": 2, "10000BONKUSDT": -3, "BTC": 4}));
        assert!(project(&config, &rules, None, &input, 2.0, 1_000_001).is_err());
        let translated = project(&config, &rules, Some(&prefixes), &input, 2.0, 1_000_001).unwrap();
        assert_eq!(translated.targets["PEPEUSDT"].qty, 4000.0);
        assert_eq!(translated.targets["BONKUSDT"].qty, -60000.0);
        assert_eq!(translated.targets["BTC"].qty, 8.0);
        assert_eq!(translated.rules_fetched_at_us, 950_000);
        prefixes.mappings.remove("1000PEPEUSDT");
        assert!(project(&config, &rules, Some(&prefixes), &input, 2.0, 1_000_001).is_err());

        let mut exact = rules.symbols["PEPEUSDT"].clone();
        exact.base_asset = "1000PEPE".into();
        rules.symbols.insert("1000PEPEUSDT".into(), exact);
        let translated = project(&config, &rules, Some(&prefixes), &input, 2.0, 1_000_001).unwrap();
        assert_eq!(translated.targets["1000PEPEUSDT"].qty, 4.0);
        assert!(!translated.targets.contains_key("PEPEUSDT"));
    }

    #[test]
    fn rejects_stale_provenance_collisions_and_invalid_arithmetic() {
        let input = targets(serde_json::json!({"BTCUSDT":1}));
        let config = TargetProxyConfig::default();
        for now in [999_999, 181_000_001] {
            assert!(project(&config, &snapshot(), None, &input, 1.0, now).is_err());
        }
        let mut bad = snapshot();
        bad.execution_backend = "ltp".into();
        assert!(project(&config, &bad, None, &input, 1.0, 2_000_000).is_err());
        assert!(project(&config, &snapshot(), None, &input, f64::INFINITY, 2_000_000).is_err());
        let overflow = targets(serde_json::json!({"BTCUSDT":f64::MAX}));
        assert!(project(&config, &snapshot(), None, &overflow, 2.0, 2_000_000).is_err());
        let mut config = TargetProxyConfig {
            symbol_mappings: BTreeMap::from([(
                "PEPEUSDT".into(),
                ContractMapping {
                    symbol: "1000PEPEUSDT".into(),
                    source_contract_multiplier: 1.0,
                    target_contract_multiplier: 1000.0,
                },
            )]),
            ..Default::default()
        };
        let collision = targets(serde_json::json!({"PEPEUSDT":2,"1000PEPEUSDT":3}));
        assert!(project(&config, &snapshot(), None, &collision, 1.0, 2_000_000).is_err());
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
