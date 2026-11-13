//! Read-only domestic execution observation; no SDK, exchange or target transport.
pub mod config;
pub mod reader;
pub mod state;
pub mod web;

pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

pub fn validate_market(market: &str) -> anyhow::Result<()> {
    let (venue, symbol) = market
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("target must be SHFE:contract or INE:contract"))?;
    anyhow::ensure!(
        matches!(venue, "SHFE" | "INE"),
        "unsupported Yida market: {venue}"
    );
    let prefix = symbol.bytes().take_while(u8::is_ascii_lowercase).count();
    anyhow::ensure!(
        (1..=3).contains(&prefix) && symbol.len() >= prefix + 4 && symbol.len() < 64,
        "invalid native contract: {symbol}"
    );
    let month = &symbol.as_bytes()[prefix..prefix + 4];
    anyhow::ensure!(
        month.iter().all(u8::is_ascii_digit)
            && (1..=12).contains(&((month[2] - b'0') * 10 + month[3] - b'0')),
        "invalid contract month: {symbol}"
    );
    let tail = &symbol[prefix + 4..];
    anyhow::ensure!(
        tail.is_empty()
            || (matches!(tail.as_bytes()[0], b'C' | b'P')
                && tail.len() > 1
                && tail[1..].bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && tail[1..]
                    .parse::<f64>()
                    .is_ok_and(|x| x.is_finite() && x > 0.0)),
        "invalid option contract: {symbol}"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
