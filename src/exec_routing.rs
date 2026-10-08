use anyhow::{Result, bail};

use crate::config::SourceConfig;

/// Each Exec deployment owns exactly its configured market.
pub(crate) fn markets(source: &SourceConfig) -> Result<Vec<String>> {
    let values = crate::market_rules::source_values(source)?;
    let backend = crate::market_rules::execution_backend(&source.venue, &values)?;
    anyhow::ensure!(
        source.venue != "binance-coin-futures" || backend == "native",
        "COIN-M execution requires the native Binance backend"
    );
    Ok(vec![source.venue.clone()])
}

pub(crate) fn symbol_market(configured_venue: &str, symbol: &str) -> Result<&'static str> {
    let symbol = crate::order_config::normalize_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    match configured_venue {
        "binance-futures" if symbol.ends_with("USDT") || symbol.ends_with("USDC") => {
            Ok("binance-futures")
        }
        "binance-coin-futures" if symbol.ends_with("USD") => {
            crate::order_config::binance_coin_wire_symbol(&symbol).map_err(anyhow::Error::msg)?;
            Ok("binance-coin-futures")
        }
        "okex-futures" => Ok("okex-futures"),
        _ => bail!(
            "symbol {symbol} does not belong to Exec market {configured_venue}; use a separate Exec deployment for the other market"
        ),
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
        assert_eq!(markets(&source).unwrap(), vec!["binance-futures"]);
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
        assert!(symbol_market("binance-futures", "BTC").is_err());
    }
}
