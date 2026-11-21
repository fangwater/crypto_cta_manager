//! Optional cross-market facts appended after the stored trade-update and
//! uniform-order layouts. Crypto producers never write the block; domestic
//! futures always do. Layout (little endian): `u32 EXTENSION_MAGIC`, `u8 flags`,
//! then present fields in flag order: position effect `u8`; trading day and
//! trade id as `u16 len + UTF-8`; last filled quantity and commission `f64`;
//! client order text as `u16 len + UTF-8`. The magic is larger than any crypto
//! uniform provenance length, so the two tails cannot be confused.
use super::Decoder;
use anyhow::{Context, Result, bail, ensure};

pub const EXTENSION_MAGIC: u32 = 0xFFFF_FFFE;

const POSITION_EFFECT: u8 = 1 << 0;
const TRADING_DAY: u8 = 1 << 1;
const TRADE_ID: u8 = 1 << 2;
const LAST_FILLED: u8 = 1 << 3;
const COMMISSION: u8 = 1 << 4;
const CLIENT_ORDER_TEXT: u8 = 1 << 5;
const KNOWN: u8 = (1 << 6) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEffect {
    Open = 1,
    Close = 2,
    CloseToday = 3,
    CloseYesterday = 4,
}

impl PositionEffect {
    pub fn from_code(code: u8) -> Result<Self> {
        Ok(match code {
            1 => Self::Open,
            2 => Self::Close,
            3 => Self::CloseToday,
            4 => Self::CloseYesterday,
            _ => bail!("invalid record extension position effect {code}"),
        })
    }
}

/// Decoded domestic facts of one stored record; absent fields stay `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordExtension {
    pub position_effect: Option<PositionEffect>,
    pub trading_day: Option<String>,
    pub trade_id: Option<String>,
    pub last_filled_quantity: Option<f64>,
    pub commission: Option<f64>,
    /// Native order identity `session/order_ref`.
    pub client_order_text: Option<String>,
}

impl RecordExtension {
    /// Writes the block exactly like the Exec writer; empty blocks write nothing.
    pub fn encode(&self, out: &mut Vec<u8>) {
        if *self == Self::default() {
            return;
        }
        let flag = |set: bool, bit: u8| if set { bit } else { 0 };
        let flags = flag(self.position_effect.is_some(), POSITION_EFFECT)
            | flag(self.trading_day.is_some(), TRADING_DAY)
            | flag(self.trade_id.is_some(), TRADE_ID)
            | flag(self.last_filled_quantity.is_some(), LAST_FILLED)
            | flag(self.commission.is_some(), COMMISSION)
            | flag(self.client_order_text.is_some(), CLIENT_ORDER_TEXT);
        out.extend_from_slice(&EXTENSION_MAGIC.to_le_bytes());
        out.push(flags);
        if let Some(effect) = self.position_effect {
            out.push(effect as u8);
        }
        let text = |out: &mut Vec<u8>, value: &str| {
            out.extend_from_slice(&(value.len() as u16).to_le_bytes());
            out.extend_from_slice(value.as_bytes());
        };
        for value in [&self.trading_day, &self.trade_id].into_iter().flatten() {
            text(out, value);
        }
        for value in [self.last_filled_quantity, self.commission]
            .into_iter()
            .flatten()
        {
            out.extend_from_slice(&value.to_le_bytes());
        }
        if let Some(value) = &self.client_order_text {
            text(out, value);
        }
    }
}

/// Whether the remaining payload starts with an extension block.
pub(super) fn starts(decoder: &Decoder<'_>) -> bool {
    decoder.remaining() >= 4
        && decoder.input[decoder.offset..decoder.offset + 4] == EXTENSION_MAGIC.to_le_bytes()
}

/// Reads one block; the caller has checked [`starts`].
pub(super) fn read(decoder: &mut Decoder<'_>) -> Result<RecordExtension> {
    ensure!(
        decoder.u32("extension magic")? == EXTENSION_MAGIC,
        "missing record extension magic"
    );
    let flags = decoder.u8("extension flags")?;
    ensure!(
        flags & !KNOWN == 0,
        "unknown record extension flags {flags:#x}"
    );
    let text = |decoder: &mut Decoder<'_>, field: &str| -> Result<String> {
        let length = usize::from(decoder.u16(field)?);
        Ok(std::str::from_utf8(decoder.bytes(length, field)?)
            .with_context(|| format!("record extension {field} is not UTF-8"))?
            .to_owned())
    };
    let mut value = RecordExtension::default();
    if flags & POSITION_EFFECT != 0 {
        value.position_effect = Some(PositionEffect::from_code(decoder.u8("position_effect")?)?);
    }
    if flags & TRADING_DAY != 0 {
        value.trading_day = Some(text(decoder, "trading_day")?);
    }
    if flags & TRADE_ID != 0 {
        value.trade_id = Some(text(decoder, "trade_id")?);
    }
    if flags & LAST_FILLED != 0 {
        value.last_filled_quantity = Some(decoder.f64("last_filled_quantity")?);
    }
    if flags & COMMISSION != 0 {
        value.commission = Some(decoder.f64("commission")?);
    }
    if flags & CLIENT_ORDER_TEXT != 0 {
        value.client_order_text = Some(text(decoder, "client_order_text")?);
    }
    Ok(value)
}
