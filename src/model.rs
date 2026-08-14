use anyhow::{Context, Result, anyhow, bail};

pub const UNIFORM_ORDERS_CF: &str = "uniform_orders";
pub const TRADE_UPDATES_CF: &str = "trade_updates";
pub const TRADE_UPDATES_UNMATCHED_CF: &str = "trade_updates_unmatched";
pub const ORDER_UPDATES_CF: &str = "order_updates";
pub const ORDER_UPDATES_UNMATCHED_CF: &str = "order_updates_unmatched";
const SIGNAL_BBO_LEG_BINARY_LEN: usize = 41;
const SIGNAL_BBO_BINARY_LEN: usize = 83;

#[derive(Debug, Clone, PartialEq)]
pub struct UniformOrderEvent {
    pub record_key: String,
    pub event_ts_us: i64,
    pub recv_ts_us: i64,
    pub symbol: String,
    pub create_ts_us: i64,
    pub update_ts_us: i64,
    pub signal_ts_us: i64,
    pub submit_ts_us: i64,
    pub local_ts_us: i64,
    pub market_ts_us: i64,
    pub client_order_id: i64,
    pub venue_code: i16,
    pub venue: String,
    pub order_type_code: i16,
    pub order_type: String,
    pub side_code: i16,
    pub side: String,
    pub price: f64,
    pub price_offset: f64,
    pub amount_initial: f64,
    pub amount_update: f64,
    pub status_code: i16,
    pub status: String,
    pub from_key: Vec<u8>,
    pub from_key_text: String,
    pub bbo_spread: String,
    pub signal_open: Option<SignalBboLeg>,
    pub signal_hedge: Option<SignalBboLeg>,
    pub wire_payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalBboLeg {
    pub venue_code: i16,
    pub ts_us: i64,
    pub bid_price: f64,
    pub bid_quantity: f64,
    pub ask_price: f64,
    pub ask_quantity: f64,
}

#[derive(Debug, Clone)]
pub struct DecodeFailure {
    pub record_key: Vec<u8>,
    pub wire_payload: Vec<u8>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TradeUpdateEvent {
    pub record_key: String,
    pub record_ts_us: i64,
    pub recv_ts_us: i64,
    pub event_ts_us: i64,
    pub trade_ts_us: i64,
    pub symbol: String,
    pub order_id: i64,
    pub client_order_id: i64,
    pub side_code: i16,
    pub price: f64,
    pub is_maker: bool,
    pub venue_code: i16,
    pub cumulative_filled_quantity: f64,
    pub status_code: Option<i16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderUpdateEvent {
    pub record_key: String,
    pub record_ts_us: i64,
    pub recv_ts_us: i64,
    pub event_ts_us: i64,
    pub symbol: String,
    pub order_id: i64,
    pub client_order_id: i64,
    pub client_order_id_text: Option<String>,
    pub side_code: i16,
    pub order_type_code: i16,
    pub time_in_force_code: i16,
    pub price: f64,
    pub quantity: f64,
    pub cumulative_filled_quantity: f64,
    pub status_code: i16,
    pub raw_status: String,
    pub execution_type_code: i16,
    pub raw_execution_type: String,
    pub venue_code: i16,
}

pub fn decode_trade_update(key: &[u8], payload: &[u8]) -> Result<TradeUpdateEvent> {
    let (record_key, record_ts_us) = decode_timestamp_key(key, "trade update")?;
    let mut decoder = Decoder::new(payload);
    let event = TradeUpdateEvent {
        record_key,
        record_ts_us,
        recv_ts_us: decoder.i64("recv_ts_us")?,
        event_ts_us: decoder.i64("event_time")?,
        trade_ts_us: decoder.i64("trade_time")?,
        symbol: decoder.string("symbol")?,
        order_id: decoder.i64("order_id")?,
        client_order_id: decoder.i64("client_order_id")?,
        side_code: i16::from(decoder.u8("side")?),
        price: decoder.f64("price")?,
        is_maker: decoder.u8("is_maker")? != 0,
        venue_code: i16::from(decoder.u8("venue")?),
        cumulative_filled_quantity: decoder.f64("cumulative_filled_quantity")?,
        status_code: match decoder.u8("status_present")? {
            0 => None,
            1 => Some(i16::from(decoder.u8("status")?)),
            value => bail!("trade update has invalid status-present flag {value}"),
        },
    };
    if decoder.remaining() != 0 {
        bail!(
            "trade update payload has {} trailing bytes",
            decoder.remaining()
        );
    }
    Ok(event)
}

pub fn decode_order_update(key: &[u8], payload: &[u8]) -> Result<OrderUpdateEvent> {
    let (record_key, record_ts_us) = decode_timestamp_key(key, "order update")?;
    let mut decoder = Decoder::new(payload);
    let event = OrderUpdateEvent {
        record_key,
        record_ts_us,
        recv_ts_us: decoder.i64("recv_ts_us")?,
        event_ts_us: decoder.i64("event_time")?,
        symbol: decoder.string("symbol")?,
        order_id: decoder.i64("order_id")?,
        client_order_id: decoder.i64("client_order_id")?,
        client_order_id_text: decoder.optional_string("client_order_id_str")?,
        side_code: i16::from(decoder.u8("side")?),
        order_type_code: i16::from(decoder.u8("order_type")?),
        time_in_force_code: i16::from(decoder.u8("time_in_force")?),
        price: decoder.f64("price")?,
        quantity: decoder.f64("quantity")?,
        cumulative_filled_quantity: decoder.f64("cumulative_filled_quantity")?,
        status_code: i16::from(decoder.u8("status")?),
        raw_status: decoder.string("raw_status")?,
        execution_type_code: i16::from(decoder.u8("execution_type")?),
        raw_execution_type: decoder.string("raw_execution_type")?,
        venue_code: i16::from(decoder.u8("venue")?),
    };
    if decoder.remaining() != 0 {
        bail!(
            "order update payload has {} trailing bytes",
            decoder.remaining()
        );
    }
    Ok(event)
}

fn decode_timestamp_key(key: &[u8], kind: &str) -> Result<(String, i64)> {
    let record_key = std::str::from_utf8(key)
        .with_context(|| format!("{kind} key is not UTF-8"))?
        .to_string();
    if record_key.len() != 20 || !record_key.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("{kind} key is not a 20-digit timestamp: {record_key:?}");
    }
    let record_ts_us = record_key
        .parse::<i64>()
        .with_context(|| format!("{kind} key exceeds i64"))?;
    Ok((record_key, record_ts_us))
}

pub fn decode_uniform_order(key: &[u8], payload: &[u8]) -> Result<UniformOrderEvent> {
    let record_key = std::str::from_utf8(key)
        .context("uniform order key is not UTF-8")?
        .to_string();
    if record_key.len() != 20 || !record_key.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("uniform order key is not a 20-digit timestamp: {record_key:?}");
    }
    let event_ts_us = record_key
        .parse::<i64>()
        .context("uniform order key exceeds PostgreSQL bigint")?;

    let mut decoder = Decoder::new(payload);
    let recv_ts_us = decoder.i64("recv_ts_us")?;
    let symbol_len = usize::from(decoder.u16("symbol_len")?);
    let symbol = decoder.lossy_string(symbol_len, "symbol")?;
    let create_ts_us = decoder.i64("create_ts")?;
    let update_ts_us = decoder.i64("update_ts")?;
    let signal_ts_us = decoder.i64("signal_ts")?;
    let submit_ts_us = decoder.i64("submit_ts")?;
    let local_ts_us = decoder.i64("local_ts")?;
    let market_ts_us = decoder.i64("mkt_ts")?;
    let client_order_id = decoder.i64("client_order_id")?;
    let venue_raw = decoder.u8("venue")?;
    let order_type_raw = decoder.u8("order_type")?;
    let side_raw = decoder.u8("side")?;
    let price = decoder.f64("price")?;
    let price_offset = decoder.f64("price_offset")?;
    let amount_initial = decoder.f64("amount_init")?;
    let amount_update = decoder.f64("amount_update")?;
    let status_raw = decoder.u8("status")?;
    let from_key_len = decoder.u32("from_key_len")? as usize;
    let from_key = decoder.bytes(from_key_len, "from_key")?.to_vec();
    let from_key_text = String::from_utf8_lossy(&from_key).into_owned();

    let bbo_spread = if decoder.remaining() == 0 {
        String::new()
    } else {
        let length = usize::from(decoder.u16("bbo_spread_len")?);
        decoder.lossy_string(length, "bbo_spread")?
    };

    let (signal_open, signal_hedge) = match decoder.remaining() {
        0 => (None, None),
        SIGNAL_BBO_BINARY_LEN => {
            decode_signal_bbo(decoder.bytes(SIGNAL_BBO_BINARY_LEN, "signal_bbo")?)?
        }
        remaining => {
            bail!("uniform order signal_bbo must be {SIGNAL_BBO_BINARY_LEN} bytes, got {remaining}")
        }
    };
    if decoder.remaining() != 0 {
        bail!(
            "uniform order payload has {} trailing bytes",
            decoder.remaining()
        );
    }

    Ok(UniformOrderEvent {
        record_key,
        event_ts_us,
        recv_ts_us,
        symbol,
        create_ts_us,
        update_ts_us,
        signal_ts_us,
        submit_ts_us,
        local_ts_us,
        market_ts_us,
        client_order_id,
        venue_code: i16::from(venue_raw),
        venue: venue_name(venue_raw),
        order_type_code: i16::from(order_type_raw),
        order_type: order_type_name(order_type_raw),
        side_code: i16::from(side_raw),
        side: side_name(side_raw),
        price,
        price_offset,
        amount_initial,
        amount_update,
        status_code: i16::from(status_raw),
        status: status_name(status_raw),
        from_key,
        from_key_text,
        bbo_spread,
        signal_open,
        signal_hedge,
        wire_payload: payload.to_vec(),
    })
}

fn decode_signal_bbo(raw: &[u8]) -> Result<(Option<SignalBboLeg>, Option<SignalBboLeg>)> {
    let mask = raw[0];
    if mask & !0b11 != 0 {
        bail!("invalid signal_bbo presence mask: {mask}");
    }
    let open = decode_signal_leg(&raw[1..1 + SIGNAL_BBO_LEG_BINARY_LEN], mask & 1 != 0)?;
    let hedge = decode_signal_leg(&raw[1 + SIGNAL_BBO_LEG_BINARY_LEN..], mask & 2 != 0)?;
    Ok((open, hedge))
}

fn decode_signal_leg(raw: &[u8], present: bool) -> Result<Option<SignalBboLeg>> {
    if !present {
        return Ok(None);
    }
    if raw.len() != SIGNAL_BBO_LEG_BINARY_LEN {
        bail!("invalid signal_bbo leg length: {}", raw.len());
    }
    Ok(Some(SignalBboLeg {
        venue_code: i16::from(raw[0]),
        ts_us: i64::from_le_bytes(raw[1..9].try_into()?),
        bid_price: f64::from_le_bytes(raw[9..17].try_into()?),
        bid_quantity: f64::from_le_bytes(raw[17..25].try_into()?),
        ask_price: f64::from_le_bytes(raw[25..33].try_into()?),
        ask_quantity: f64::from_le_bytes(raw[33..41].try_into()?),
    }))
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.offset)
    }

    fn bytes(&mut self, length: usize, field: &str) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| anyhow!("{field} length overflow"))?;
        let value = self.input.get(self.offset..end).ok_or_else(|| {
            anyhow!(
                "payload too short for {field}: need {length}, have {}",
                self.remaining()
            )
        })?;
        self.offset = end;
        Ok(value)
    }

    fn lossy_string(&mut self, length: usize, field: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(self.bytes(length, field)?).into_owned())
    }

    fn string(&mut self, field: &str) -> Result<String> {
        let length = self.u32(&format!("{field}_len"))? as usize;
        self.lossy_string(length, field)
    }

    fn optional_string(&mut self, field: &str) -> Result<Option<String>> {
        match self.u8(&format!("{field}_present"))? {
            0 => Ok(None),
            1 => self.string(field).map(Some),
            value => bail!("{field} has invalid present flag {value}"),
        }
    }

    fn u8(&mut self, field: &str) -> Result<u8> {
        Ok(self.bytes(1, field)?[0])
    }

    fn u16(&mut self, field: &str) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2, field)?.try_into()?))
    }

    fn u32(&mut self, field: &str) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4, field)?.try_into()?))
    }

    fn i64(&mut self, field: &str) -> Result<i64> {
        Ok(i64::from_le_bytes(self.bytes(8, field)?.try_into()?))
    }

    fn f64(&mut self, field: &str) -> Result<f64> {
        Ok(f64::from_le_bytes(self.bytes(8, field)?.try_into()?))
    }
}

pub fn venue_name(value: u8) -> String {
    match value {
        0 => "BinanceMargin",
        1 => "BinanceFutures",
        2 => "OkexMargin",
        3 => "OkexFutures",
        4 => "BybitMargin",
        5 => "BybitFutures",
        6 => "BitgetMargin",
        7 => "BitgetFutures",
        8 => "GateMargin",
        9 => "GateFutures",
        10 => "AsterMargin",
        11 => "AsterFutures",
        12 => "HyperliquidMargin",
        13 => "HyperliquidFutures",
        _ => return format!("UNKNOWN({value})"),
    }
    .to_string()
}

fn order_type_name(value: u8) -> String {
    match value {
        1 => "LIMIT",
        3 => "MARKET",
        4 => "STOP_LOSS",
        5 => "STOP_LOSS_LIMIT",
        6 => "TAKE_PROFIT",
        7 => "TAKE_PROFIT_LIMIT",
        8 => "STOP_MARKET",
        9 => "TAKE_PROFIT_MARKET",
        _ => return format!("UNKNOWN({value})"),
    }
    .to_string()
}

fn side_name(value: u8) -> String {
    match value {
        1 => "BUY".to_string(),
        2 => "SELL".to_string(),
        _ => format!("UNKNOWN({value})"),
    }
}

fn status_name(value: u8) -> String {
    match value {
        1 => "NEW",
        2 => "PARTIALLY_FILLED",
        3 => "FILLED",
        4 => "CANCELED",
        5 => "EXPIRED",
        6 => "EXPIRED_IN_MATCH",
        _ => return format!("UNKNOWN({value})"),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_leg(out: &mut Vec<u8>, venue: u8, ts: i64, prices: [f64; 4]) {
        out.push(venue);
        out.extend_from_slice(&ts.to_le_bytes());
        for value in prices {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn fixture() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1_000_i64.to_le_bytes());
        out.extend_from_slice(&7_u16.to_le_bytes());
        out.extend_from_slice(b"BTCUSDT");
        for value in [10_i64, 11, 12, 13, 14, 15, 16] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&[1, 1, 2]);
        for value in [100.0_f64, 0.1, 2.0, 0.5] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.push(3);
        out.extend_from_slice(&4_u32.to_le_bytes());
        out.extend_from_slice(b"open");
        out.extend_from_slice(&3_u16.to_le_bytes());
        out.extend_from_slice(b"bbo");
        out.push(0b11);
        push_leg(&mut out, 1, 20, [99.0, 1.0, 100.0, 2.0]);
        push_leg(&mut out, 0, 21, [98.0, 3.0, 101.0, 4.0]);
        out
    }

    #[test]
    fn decodes_v2_uniform_order_with_bbo() {
        let event = decode_uniform_order(b"00000000000000001000", &fixture()).unwrap();
        assert_eq!(event.event_ts_us, 1_000);
        assert_eq!(event.symbol, "BTCUSDT");
        assert_eq!(event.client_order_id, 16);
        assert_eq!(event.venue, "BinanceFutures");
        assert_eq!(event.side, "SELL");
        assert_eq!(event.status, "FILLED");
        assert_eq!(event.from_key, b"open");
        assert_eq!(event.bbo_spread, "bbo");
        assert_eq!(event.signal_open.unwrap().bid_price, 99.0);
        assert_eq!(event.signal_hedge.unwrap().ask_quantity, 4.0);
    }

    #[test]
    fn rejects_non_timestamp_key() {
        let error = decode_uniform_order(b"bad", &fixture()).unwrap_err();
        assert!(error.to_string().contains("20-digit"));
    }

    #[test]
    fn decodes_historical_record_without_optional_bbo_tail() {
        let mut payload = fixture();
        payload.truncate(payload.len() - (2 + 3 + SIGNAL_BBO_BINARY_LEN));
        let event = decode_uniform_order(b"00000000000000001000", &payload).unwrap();
        assert_eq!(event.from_key, b"open");
        assert!(event.bbo_spread.is_empty());
        assert!(event.signal_open.is_none());
        assert!(event.signal_hedge.is_none());
    }
}
