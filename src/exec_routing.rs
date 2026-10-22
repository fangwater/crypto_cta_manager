use anyhow::{Result, bail};

use crate::config::SourceConfig;

pub(crate) fn validate_grid_account(
    source: &SourceConfig,
    parameters: &crate::order_config::OrderParameters,
    overrides: &std::collections::BTreeMap<String, crate::order_config::OrderParameters>,
) -> Result<()> {
    use crate::order_config::ExecutionAlgorithm;
    if !std::iter::once(parameters)
        .chain(overrides.values())
        .any(|p| p.algorithm == ExecutionAlgorithm::Grid)
    {
        return Ok(());
    }
    let values = crate::market_rules::source_values(source)?;
    let backend = crate::market_rules::execution_backend(&source.venue, &values)?;
    let allowed = match (backend, source.venue.as_str()) {
        ("ltp", "binance-futures" | "okex-futures") => true,
        ("native", "binance-futures") => values.get("BINANCE_ACCOUNT_MODE").is_some_and(|mode| {
            mode.trim().eq_ignore_ascii_case("STANDARD")
                || mode.trim().eq_ignore_ascii_case("UNIFIED")
        }),
        ("native", "binance-coin-futures") => values
            .get("BINANCE_ACCOUNT_MODE")
            .is_some_and(|mode| mode.trim().eq_ignore_ascii_case("UNIFIED")),
        // Native Exec startup verifies these supported unified account modes.
        ("native", "okex-futures" | "bybit-futures" | "bitget-futures") => true,
        _ => false,
    };
    anyhow::ensure!(
        allowed,
        "Grid requires RapidX, native Binance USD-M, or a supported native unified account: {}",
        source.id
    );
    Ok(())
}

/// RapidX linear Execs own a perpetual market and its USDT cash market.
pub(crate) fn markets(source: &SourceConfig) -> Result<Vec<String>> {
    let values = crate::market_rules::source_values(source)?;
    let backend = crate::market_rules::execution_backend(&source.venue, &values)?;
    anyhow::ensure!(
        source.venue != "binance-coin-futures" || backend == "native",
        "COIN-M execution requires the native Binance backend"
    );
    let mut markets = vec![source.venue.clone()];
    if backend == "ltp" {
        match source.venue.as_str() {
            "binance-futures" => markets.push("binance-margin".into()),
            "okex-futures" => markets.push("okex-margin".into()),
            _ => (),
        }
    }
    Ok(markets)
}

pub(crate) fn symbol_market(configured_venue: &str, symbol: &str) -> Result<&'static str> {
    let symbol = crate::order_config::normalize_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    match configured_venue {
        "bybit-futures" if symbol.ends_with("USDT") => Ok("bybit-futures"),
        "bitget-futures" if symbol.ends_with("USDT") => Ok("bitget-futures"),
        "binance-futures" if symbol.ends_with("USDT") || symbol.ends_with("USDC") => {
            Ok("binance-futures")
        }
        "binance-coin-futures" if symbol.ends_with("USD") => {
            crate::order_config::binance_coin_wire_symbol(&symbol).map_err(anyhow::Error::msg)?;
            Ok("binance-coin-futures")
        }
        "okex-futures" if symbol.ends_with("USDT") || symbol.ends_with("USDC") => {
            Ok("okex-futures")
        }
        "binance-futures" | "binance-margin" if is_spot_target(&symbol) => Ok("binance-margin"),
        "okex-futures" | "okex-margin" if is_spot_target(&symbol) => Ok("okex-margin"),
        _ => bail!(
            "symbol {symbol} does not belong to Exec market {configured_venue}; use a separate Exec deployment for the other market"
        ),
    }
}

/// Proxy inputs may include unsupported markets; the account projection filters
/// them before runtime routing. Ordinary accounts retain strict venue checks.
pub(crate) fn validate_target_symbol(source: &SourceConfig, symbol: &str) -> Result<()> {
    if source.target_proxy.is_some() {
        crate::order_config::validate_exec_symbol(symbol).map_err(anyhow::Error::msg)
    } else {
        symbol_market(&source.venue, symbol).map(|_| ())
    }
}

pub(crate) fn for_symbol(source: &SourceConfig, symbol: &str) -> Result<SourceConfig> {
    let market = symbol_market(&source.venue, symbol)?;
    anyhow::ensure!(
        markets(source)?.iter().any(|venue| venue == market),
        "source {} does not support {market}",
        source.id
    );
    let mut selected = source.clone();
    selected.venue = market.into();
    Ok(selected)
}

pub(crate) fn is_spot_target(symbol: &str) -> bool {
    !symbol.ends_with("USDT") && !symbol.ends_with("USDC") && !symbol.ends_with("USD")
}

/// Expose cash fills using the same bare-asset identity as published targets.
/// Event venue remains an independent part of the FIFO key.
pub(crate) fn target_symbol(symbol: &str, venue_code: i16) -> String {
    if matches!(venue_code, 0 | 2) {
        symbol.strip_suffix("USDT").unwrap_or(symbol).to_string()
    } else {
        symbol.to_string()
    }
}

/// Historical archives retain their original source identity and market facts.
/// Current deployment capabilities must not reinterpret or invalidate old targets.
pub(crate) fn archived_symbol_market(configured_venue: &str, symbol: &str) -> Result<&'static str> {
    let normalized =
        crate::order_config::normalize_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    let venue = if configured_venue.starts_with("binance-") && normalized.ends_with("USD") {
        "binance-coin-futures"
    } else if configured_venue.starts_with("binance-") {
        "binance-futures"
    } else {
        configured_venue
    };
    symbol_market(venue, &normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn grid_binding_gate_covers_base_and_symbol_algorithms() {
        use crate::order_config::{ExecutionAlgorithm, OrderParameters};
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("fixture.env");
        let mut source: SourceConfig = toml::from_str(&format!("id='fixture'\naccount='fixture'\nvenue='binance-futures'\nrocksdb_path='{}'\nenv_path='{}'\n", dir.path().join("data/persist_manager").display(), env.display())).unwrap();
        let base = OrderParameters::default();
        let grid = OrderParameters {
            algorithm: ExecutionAlgorithm::Grid,
            ..base.clone()
        };
        let overrides = BTreeMap::from([("BTCUSDT".into(), grid.clone())]);
        for body in [
            "TRADE_ENGINE_EXEC_BACKEND=native\n",
            "BINANCE_ACCOUNT_MODE=invalid\n",
        ] {
            std::fs::write(&env, body).unwrap();
            assert!(validate_grid_account(&source, &base, &BTreeMap::new()).is_ok());
            assert!(validate_grid_account(&source, &grid, &BTreeMap::new()).is_err());
            assert!(validate_grid_account(&source, &base, &overrides).is_err());
        }
        for body in [
            "BINANCE_ACCOUNT_MODE=UNIFIED\n",
            "BINANCE_ACCOUNT_MODE=STANDARD\n",
            "BINANCE_ACCOUNT_MODE=' standard '\n",
            "TRADE_ENGINE_EXEC_BACKEND_MAP='binance=rapidx'\n",
        ] {
            std::fs::write(&env, body).unwrap();
            assert!(validate_grid_account(&source, &grid, &BTreeMap::new()).is_ok());
            assert!(validate_grid_account(&source, &base, &overrides).is_ok());
        }
        std::fs::write(&env, "BINANCE_ACCOUNT_MODE=STANDARD\n").unwrap();
        assert_eq!(markets(&source).unwrap(), vec!["binance-futures"]);
        assert!(for_symbol(&source, "BTCUSDT").is_ok());
        assert!(for_symbol(&source, "BTC").is_err());
        assert!(for_symbol(&source, "BTCUSD").is_err());
        source.venue = "binance-coin-futures".into();
        assert!(validate_grid_account(&source, &grid, &BTreeMap::new()).is_err());
        assert!(validate_grid_account(&source, &base, &overrides).is_err());
        std::fs::write(&env, "BINANCE_ACCOUNT_MODE=UNIFIED\n").unwrap();
        assert!(validate_grid_account(&source, &grid, &BTreeMap::new()).is_ok());
        std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND=native\n").unwrap();
        for venue in ["okex-futures", "bybit-futures", "bitget-futures"] {
            source.venue = venue.into();
            assert!(validate_grid_account(&source, &grid, &BTreeMap::new()).is_ok());
        }
    }

    #[test]
    fn native_uta_sources_keep_usdt_market_and_account_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        for (venue, service) in [
            ("bybit-futures", "bybit_pm"),
            ("bitget-futures", "bitget_pm"),
        ] {
            let source: SourceConfig = toml::from_str(&format!("id='uta_test'\naccount='test'\nvenue='{venue}'\nrocksdb_path='{}'\nipc_namespace='isolated'\n", dir.path().join("data/persist_manager").display())).unwrap();
            assert_eq!(markets(&source).unwrap(), vec![venue]);
            assert_eq!(for_symbol(&source, "BTCUSDT").unwrap().id, "uta_test");
            assert_eq!(
                source.account_ipc_service_name().unwrap(),
                format!("isolated/account_pubs/{service}")
            );
            for symbol in ["BTC", "BTCUSD", "BTCUSDC", "BTCPERP", "BTCUSDT_261225"] {
                assert!(for_symbol(&source, symbol).is_err());
            }
            let mut explicit = source.clone();
            explicit.account_ipc_service = Some("account_pubs/custom".into());
            assert_eq!(
                explicit.account_ipc_service_name().unwrap(),
                "isolated/account_pubs/custom"
            );
            let values = BTreeMap::from([("TRADE_ENGINE_EXEC_BACKEND".into(), "ltp".into())]);
            assert!(crate::market_rules::execution_backend(venue, &values).is_err());
        }
    }

    #[test]
    fn deployment_market_is_fixed_for_every_backend() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("fixture.env");
        let source: SourceConfig = toml::from_str(&format!("id='fixture'\naccount='fixture'\nvenue='binance-futures'\nrocksdb_path='{}'\nenv_path='{}'\n", dir.path().join("data/persist_manager").display(), env.display())).unwrap();
        std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND=native\n").unwrap();
        assert_eq!(markets(&source).unwrap(), vec!["binance-futures"]);
        assert!(for_symbol(&source, "BTCUSD").is_err());
        let mut coin_source = source.clone();
        coin_source.venue = "binance-coin-futures".into();
        let coin = for_symbol(&coin_source, "BTCUSD").unwrap();
        assert_eq!(coin.id, source.id);
        assert_eq!(coin.env_path, source.env_path);
        assert_eq!(coin.venue, "binance-coin-futures");
        std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND_MAP='binance=rapidx'\n").unwrap();
        assert_eq!(
            markets(&source).unwrap(),
            vec!["binance-futures", "binance-margin"]
        );
        assert_eq!(for_symbol(&source, "BTC").unwrap().venue, "binance-margin");
        assert!(for_symbol(&source, "BTCUSD").is_err());
        assert!(for_symbol(&source, "BTCUSDC").is_ok());
        assert!(markets(&coin_source).is_err());
    }

    #[test]
    fn perpetual_quote_routes_linear_and_coin_symbols_without_delivery_aliases() {
        for symbol in ["BTCUSDT", "BTCUSDC"] {
            assert_eq!(
                symbol_market("binance-futures", symbol).unwrap(),
                "binance-futures"
            );
        }
        for symbol in ["BTCUSD", "btcusd_perp", "BTCUSDPERP"] {
            assert_eq!(
                symbol_market("binance-coin-futures", symbol).unwrap(),
                "binance-coin-futures"
            );
        }
        assert!(symbol_market("binance-futures", "BTCUSD_261225").is_err());
        assert!(symbol_market("binance-futures", "BTCUSD261225").is_err());
        assert!(symbol_market("binance-coin-futures", "BTCUSDT").is_err());
        assert!(symbol_market("binance-futures", "BTCUSD").is_err());
        assert_eq!(
            symbol_market("binance-futures", "BTC").unwrap(),
            "binance-margin"
        );
    }
}
