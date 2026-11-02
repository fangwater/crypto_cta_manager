//! Venue identity shared by public prices and cross-exchange position targets.
use anyhow::{Result, bail, ensure};

pub fn venue_code(venue: &str) -> Result<i16> {
    match venue {
        "binance-margin" => Ok(0),
        "binance-futures" => Ok(1),
        "okex-margin" => Ok(2),
        "okex-futures" => Ok(3),
        _ => bail!("unsupported public market: {venue}"),
    }
}

pub fn cache_symbol(symbol: &str, venue: i16) -> Result<String> {
    crate::order_config::validate_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    match venue {
        0 | 1 => Ok(symbol.to_owned()),
        2 => Ok(format!("okex-margin:{symbol}")),
        3 => Ok(format!("okex-futures:{symbol}")),
        _ => bail!("unsupported public market code: {venue}"),
    }
}

pub fn market_key(venue: &str, symbol: &str) -> Result<String> {
    cache_symbol(symbol, venue_code(venue)?)
}

pub fn split_cache_symbol(key: &str) -> Result<(&str, &str)> {
    let (venue, symbol) = key.split_once(':').unwrap_or_else(|| {
        (
            if crate::exec_routing::is_spot_target(key) {
                "binance-margin"
            } else {
                "binance-futures"
            },
            key,
        )
    });
    venue_code(venue)?;
    crate::order_config::validate_exec_symbol(symbol).map_err(anyhow::Error::msg)?;
    ensure!(!symbol.is_empty(), "empty market symbol");
    Ok((venue, symbol))
}

pub fn normalize_target(raw: &str) -> Result<String, String> {
    if let Some((venue, symbol)) = raw.trim().split_once(':') {
        let venue = venue.to_ascii_lowercase();
        if !matches!(venue.as_str(), "okex-futures" | "binance-futures") {
            return Err(format!("cross spread target venue is unsupported: {venue}"));
        }
        let symbol = crate::order_config::normalize_exec_symbol(symbol)?;
        if !symbol.ends_with("USDT") || symbol.len() <= 4 {
            return Err("cross spread targets require USDT perpetuals".into());
        }
        Ok(format!("{venue}:{symbol}"))
    } else {
        crate::order_config::normalize_exec_symbol(raw)
    }
}

pub fn validate_target(key: &str) -> Result<(), String> {
    if normalize_target(key)? != key {
        return Err(format!("noncanonical target: {key}"));
    }
    Ok(())
}

pub fn normalize_targets<T>(
    values: std::collections::BTreeMap<String, T>,
) -> Result<std::collections::BTreeMap<String, T>, String> {
    let mut result = std::collections::BTreeMap::new();
    for (key, value) in values {
        let key = normalize_target(&key)?;
        if result.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate target: {key}"));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_asset_has_independent_exchange_and_market_cache_keys() {
        let keys = [("BTC", 0), ("BTCUSDT", 1), ("BTC", 2), ("BTCUSDT", 3)]
            .map(|(s, v)| cache_symbol(s, v).unwrap());
        assert_eq!(
            keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
            4
        );
        assert_eq!(
            split_cache_symbol(&keys[3]).unwrap(),
            ("okex-futures", "BTCUSDT")
        );
        assert_eq!(
            normalize_target(" OKEX-FUTURES:btcusdt ").unwrap(),
            "okex-futures:BTCUSDT"
        );
        assert!(normalize_target("bybit-futures:BTCUSDT").is_err());
        assert!(normalize_target("okex-futures:BTC").is_err());
        assert!(split_cache_symbol("okex-futures:BTC:USDT").is_err());
    }
}
