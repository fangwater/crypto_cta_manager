use anyhow::{Result, bail};

use crate::config::SourceConfig;

/// A native Binance account owns both perpetual markets under one source id.
pub(crate) fn markets(source: &SourceConfig) -> Result<Vec<String>> {
    let values = crate::market_rules::source_values(source)?;
    let backend = crate::market_rules::execution_backend(&source.venue, &values)?;
    Ok(
        if source.venue == "binance-futures" && backend == "native" {
            vec!["binance-futures".into(), "binance-coin-futures".into()]
        } else {
            vec![source.venue.clone()]
        },
    )
}

pub(crate) fn symbol_market(configured_venue: &str, symbol: &str) -> Result<&'static str> {
    let symbol = crate::order_config::normalize_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    match configured_venue {
        "binance-futures" if symbol.ends_with("USDT") || symbol.ends_with("USDC") => {
            Ok("binance-futures")
        }
        "binance-futures" | "binance-coin-futures" if symbol.ends_with("USD") => {
            crate::order_config::binance_coin_wire_symbol(&symbol).map_err(anyhow::Error::msg)?;
            Ok("binance-coin-futures")
        }
        "okex-futures" => Ok("okex-futures"),
        _ => bail!(
            "invalid perpetual symbol for {configured_venue}: {symbol}; Binance targets use USDT, USDC or USD"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_backend_controls_coin_capability_without_changing_source_identity() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("fixture.env");
        let source: SourceConfig = toml::from_str(&format!("id='fixture'\naccount='fixture'\nvenue='binance-futures'\nrocksdb_path='{}'\nenv_path='{}'\n", dir.path().join("data/persist_manager").display(), env.display())).unwrap();
        std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND=native\n").unwrap();
        assert_eq!(
            markets(&source).unwrap(),
            vec!["binance-futures", "binance-coin-futures"]
        );
        let coin = for_symbol(&source, "BTCUSD").unwrap();
        assert_eq!(coin.id, source.id);
        assert_eq!(coin.env_path, source.env_path);
        assert_eq!(coin.venue, "binance-coin-futures");
        std::fs::write(&env, "TRADE_ENGINE_EXEC_BACKEND_MAP='binance=rapidx'\n").unwrap();
        assert_eq!(markets(&source).unwrap(), vec!["binance-futures"]);
        assert!(for_symbol(&source, "BTCUSD").is_err());
        assert!(for_symbol(&source, "BTCUSDC").is_ok());
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
                symbol_market("binance-futures", symbol).unwrap(),
                "binance-coin-futures"
            );
        }
        assert!(symbol_market("binance-futures", "BTCUSD_261225").is_err());
        assert!(symbol_market("binance-futures", "BTCUSD261225").is_err());
        assert!(symbol_market("binance-coin-futures", "BTCUSDT").is_err());
        assert!(symbol_market("binance-futures", "BTC").is_err());
    }
}
