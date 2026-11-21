//! Domestic observation and Manager-owned strategy attribution; no trading SDK or order transport.
pub mod config;
pub mod cost;
pub mod market_data;
pub mod nav;
pub mod netting;
pub mod reader;
pub mod records;
pub mod reference;
pub mod reference_data;
pub mod state;
pub mod sync;
pub mod web;

pub const VENUES: [&str; 6] = ["SHFE", "INE", "DCE", "CZCE", "CFFEX", "GFEX"];

pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

pub fn validate_market(market: &str) -> anyhow::Result<()> {
    let (venue, symbol) = market
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("target must be VENUE:native_contract"))?;
    anyhow::ensure!(
        VENUES.contains(&venue),
        "unsupported domestic-futures venue: {venue}"
    );
    // Preserve Exec's native identifiers: CZCE uses YMM, CFFEX uses YYMM;
    // neither case nor delivery year is inferred from a provider's query code.
    let uppercase = matches!(venue, "CZCE" | "CFFEX");
    let prefix = symbol
        .bytes()
        .take_while(|b| {
            if uppercase {
                b.is_ascii_uppercase()
            } else {
                b.is_ascii_lowercase()
            }
        })
        .count();
    let digits = if venue == "CZCE" { 3 } else { 4 };
    anyhow::ensure!(
        symbol.is_ascii()
            && (1..=3).contains(&prefix)
            && symbol.len() >= prefix + digits
            && symbol.len() < 64,
        "invalid native contract: {symbol}"
    );
    anyhow::ensure!(
        venue != "CFFEX"
            || matches!(
                &symbol[..prefix],
                "IF" | "IH" | "IC" | "IM" | "TS" | "TF" | "T" | "TL"
            ),
        "invalid CFFEX futures product: {symbol}"
    );
    let month = &symbol.as_bytes()[prefix..prefix + digits];
    anyhow::ensure!(
        month.iter().all(u8::is_ascii_digit)
            && (1..=12).contains(&((month[digits - 2] - b'0') * 10 + month[digits - 1] - b'0')),
        "invalid contract month: {symbol}"
    );
    let tail = &symbol[prefix + digits..];
    anyhow::ensure!(
        tail.is_empty()
            || (matches!(venue, "SHFE" | "INE")
                && matches!(tail.as_bytes()[0], b'C' | b'P')
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
