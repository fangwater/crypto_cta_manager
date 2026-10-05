//! Public BBO codec used only by the read-only health monitor.
pub const ASK_BID_SPREAD_MSG_TYPE: u32 = 1015;
pub const SPREAD_PAYLOAD_BYTES: usize = 128;
#[derive(Debug, Clone, PartialEq)]
pub struct AskBidQuote {
    pub symbol: String,
    pub ts_us: i64,
    pub mid: f64,
}

pub fn parse_ask_bid_spread(payload: &[u8]) -> Option<AskBidQuote> {
    if payload.len() < 8 {
        return None;
    }
    let msg_type = u32::from_le_bytes(payload.get(0..4)?.try_into().ok()?);
    if msg_type != ASK_BID_SPREAD_MSG_TYPE {
        return None;
    }
    let symbol_len = u32::from_le_bytes(payload.get(4..8)?.try_into().ok()?) as usize;
    let symbol_end = 8usize.checked_add(symbol_len)?;
    let numbers_end = symbol_end.checked_add(40)?;
    if payload.len() < numbers_end {
        return None;
    }
    let symbol = std::str::from_utf8(payload.get(8..symbol_end)?).ok()?;
    let ts_us = i64::from_le_bytes(payload.get(symbol_end..symbol_end + 8)?.try_into().ok()?);
    let bid = f64::from_le_bytes(
        payload
            .get(symbol_end + 8..symbol_end + 16)?
            .try_into()
            .ok()?,
    );
    let ask = f64::from_le_bytes(
        payload
            .get(symbol_end + 24..symbol_end + 32)?
            .try_into()
            .ok()?,
    );
    if !bid.is_finite() || !ask.is_finite() || bid <= 0.0 || ask <= 0.0 || ask < bid {
        return None;
    }
    Some(AskBidQuote {
        symbol: normalize_symbol(symbol),
        ts_us,
        mid: (bid + ask) * 0.5,
    })
}

pub fn encode_ask_bid_spread(
    symbol: &str,
    ts_us: i64,
    bid: f64,
    bid_qty: f64,
    ask: f64,
    ask_qty: f64,
) -> [u8; SPREAD_PAYLOAD_BYTES] {
    let mut payload = [0u8; SPREAD_PAYLOAD_BYTES];
    payload[0..4].copy_from_slice(&ASK_BID_SPREAD_MSG_TYPE.to_le_bytes());
    payload[4..8].copy_from_slice(&(symbol.len() as u32).to_le_bytes());
    payload[8..8 + symbol.len()].copy_from_slice(symbol.as_bytes());
    let numbers = 8 + symbol.len();
    payload[numbers..numbers + 8].copy_from_slice(&ts_us.to_le_bytes());
    payload[numbers + 8..numbers + 16].copy_from_slice(&bid.to_le_bytes());
    payload[numbers + 16..numbers + 24].copy_from_slice(&bid_qty.to_le_bytes());
    payload[numbers + 24..numbers + 32].copy_from_slice(&ask.to_le_bytes());
    payload[numbers + 32..numbers + 40].copy_from_slice(&ask_qty.to_le_bytes());
    payload
}

fn normalize_symbol(raw: &str) -> String {
    raw.chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_uppercase)
        .collect()
}
