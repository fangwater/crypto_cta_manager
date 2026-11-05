//! Read factual fill roles from the Exec's durable RapidX account journal.
//! Older `rapidx_executions` values omit execType. The journal supplements only
//! a fully matching persisted execution; it never creates an order or a fill.
use super::{Execution, FillLiquidity};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct RoleEvidence {
    execution: Execution,
    role: FillLiquidity,
}

impl RoleEvidence {
    pub(super) fn validate(&self, e: &Execution) -> Result<FillLiquidity> {
        let j = &self.execution;
        ensure!(
            j.transaction_id == e.transaction_id
                && j.order_id == e.order_id
                && j.client_order_id == e.client_order_id
                && j.symbol == e.symbol
                && j.side == e.side
                && j.timestamp_ms == e.timestamp_ms
                && number(&j.quantity)? == number(&e.quantity)?
                && number(&j.price)? == number(&e.price)?,
            "RapidX journal role conflicts with persisted execution {}",
            e.transaction_id
        );
        Ok(self.role)
    }
}

fn number(value: &str) -> Result<f64> {
    let value: f64 = value.parse().context("invalid RapidX execution number")?;
    ensure!(
        value.is_finite() && value > 0.0,
        "invalid RapidX execution number"
    );
    Ok(value)
}

#[derive(Debug, Default)]
struct JournalFile {
    identity: (u64, u64),
    offset: u64,
    roles: BTreeMap<String, RoleEvidence>,
}

#[derive(Debug, Default)]
pub(crate) struct LiquidityJournal {
    scope: Option<(PathBuf, String, String)>,
    files: BTreeMap<PathBuf, JournalFile>,
    roles: BTreeMap<String, RoleEvidence>,
}

impl LiquidityJournal {
    pub(super) fn refresh(
        &mut self,
        dir: &Path,
        portfolio: &str,
        exchange: &str,
    ) -> Result<&BTreeMap<String, RoleEvidence>> {
        let scope = (dir.to_owned(), portfolio.to_owned(), exchange.to_owned());
        if self.scope.as_ref() != Some(&scope) {
            *self = Self {
                scope: Some(scope),
                ..Self::default()
            };
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.files.clear();
                self.roles.clear();
                return Ok(&self.roles);
            }
            Err(e) => return Err(e).context("read RapidX liquidity journal directory"),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "jsonl") {
                paths.push(path);
            }
        }
        paths.sort();
        self.files.retain(|p, _| paths.binary_search(p).is_ok());
        for path in paths {
            let mut file = File::open(&path).context("open RapidX liquidity journal read-only")?;
            let metadata = file.metadata()?;
            let identity = (metadata.dev(), metadata.ino());
            let state = self.files.entry(path).or_default();
            if state.identity != identity || metadata.len() < state.offset {
                *state = JournalFile {
                    identity,
                    ..JournalFile::default()
                };
            }
            file.seek(SeekFrom::Start(state.offset))?;
            // Pin the observed length. A writer's incomplete tail is retried
            // from the same byte offset after its terminating newline arrives.
            let mut reader = BufReader::new(file.take(metadata.len() - state.offset));
            let mut line = Vec::new();
            loop {
                line.clear();
                const MAX_LINE: u64 = 8 * 1024 * 1024;
                let n = reader
                    .by_ref()
                    .take(MAX_LINE + 1)
                    .read_until(b'\n', &mut line)?;
                ensure!(n as u64 <= MAX_LINE, "RapidX journal line exceeds 8 MiB");
                if !line.ends_with(b"\n") {
                    break;
                }
                if let Some(role) = parse_role(&line, portfolio, exchange)? {
                    merge(&mut state.roles, role)?;
                }
                state.offset += n as u64;
            }
        }
        // Rebuild only the small role index, never the already-read journal.
        // This also removes observations from replaced/truncated files.
        let mut roles = BTreeMap::new();
        for file in self.files.values() {
            for role in file.roles.values() {
                merge(&mut roles, role.clone())?;
            }
        }
        self.roles = roles;
        Ok(&self.roles)
    }
}

fn merge(roles: &mut BTreeMap<String, RoleEvidence>, role: RoleEvidence) -> Result<()> {
    if let Some(old) = roles.get(&role.execution.transaction_id) {
        ensure!(
            old.validate(&role.execution)? == role.role,
            "conflicting RapidX journal fill roles"
        );
    } else {
        roles.insert(role.execution.transaction_id.clone(), role);
    }
    Ok(())
}

fn parse_role(line: &[u8], portfolio: &str, exchange: &str) -> Result<Option<RoleEvidence>> {
    // Account/position snapshots dominate the journal. Avoid decoding those on
    // cold scans; whitespace and field order do not affect this string filter.
    if !line
        .windows(b"\"execType\"".len())
        .any(|v| v == b"\"execType\"")
    {
        return Ok(None);
    }
    let row: Value = serde_json::from_slice(line).context("invalid complete RapidX journal row")?;
    let message = &row["message"];
    if !matches!(
        message["channel"].as_str(),
        Some("Trades" | "HistoricalExecution")
    ) {
        return Ok(None);
    }
    let data = &message["data"];
    if data["portfolioId"] != portfolio || data["exchangeType"] != exchange {
        return Ok(None);
    }
    let role = match data["execType"].as_str() {
        Some("MAKER") => FillLiquidity::Maker,
        Some("TAKER") => FillLiquidity::Taker,
        _ => return Ok(None),
    };
    let text = |key: &str| -> Result<String> {
        data[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .with_context(|| format!("RapidX journal role missing {key}"))
    };
    let execution = Execution {
        transaction_id: text("transactionId")?,
        order_id: text("orderId")?,
        // Manual/external fills may lack a client ID. They cannot join an
        // Exec order, but must not stop other factual observations being read.
        client_order_id: data["clientOrderId"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        symbol: text("sym")?,
        side: text("side")?,
        quantity: text("quantity")?,
        price: text("price")?,
        timestamp_ms: text("createAt")?
            .parse()
            .context("invalid journal execution timestamp")?,
        rest: message["channel"] == "HistoricalExecution",
    };
    number(&execution.quantity)?;
    number(&execution.price)?;
    ensure!(
        execution.timestamp_ms > 0,
        "invalid journal execution timestamp"
    );
    Ok(Some(RoleEvidence { execution, role }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn row(tx: &str, role: &str, portfolio: &str) -> Vec<u8> {
        let row = serde_json::json!({"message":{"channel":"Trades","data":{
            "portfolioId":portfolio,"exchangeType":"BINANCE","transactionId":tx,
            "orderId":"88","clientOrderId":"7","sym":"BINANCE_PERP_FIL_USDT",
            "side":"SELL","quantity":"2.00","price":"100.0","createAt":"100",
            "execType":role,"tradingFee":"0.123"
        }}});
        let mut bytes = serde_json::to_vec(&row).unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn reads_appended_complete_lines_and_handles_rotation_replacement_and_scope() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("a.jsonl");
        let first = row("a", "MAKER", "123");
        let second = row("b", "TAKER", "123");
        std::fs::write(&path, &first).unwrap();
        let mut cache = LiquidityJournal::default();
        assert_eq!(
            cache.refresh(temp.path(), "123", "BINANCE").unwrap().len(),
            1
        );
        assert_eq!(cache.files[&path].offset, first.len() as u64);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&second[..second.len() - 1]).unwrap();
        assert_eq!(
            cache.refresh(temp.path(), "123", "BINANCE").unwrap().len(),
            1
        );
        assert_eq!(cache.files[&path].offset, first.len() as u64);
        file.write_all(b"\n").unwrap();
        assert_eq!(
            cache.refresh(temp.path(), "123", "BINANCE").unwrap().len(),
            2
        );
        assert_eq!(
            cache.refresh(temp.path(), "123", "BINANCE").unwrap()["b"].role,
            FillLiquidity::Taker
        );
        std::fs::write(temp.path().join("b.jsonl"), row("c", "MAKER", "456")).unwrap();
        assert_eq!(
            cache.refresh(temp.path(), "123", "BINANCE").unwrap().len(),
            2
        );
        assert_eq!(
            cache.refresh(temp.path(), "456", "BINANCE").unwrap().len(),
            1
        );
        std::fs::write(temp.path().join("replacement"), row("new", "TAKER", "123")).unwrap();
        std::fs::rename(temp.path().join("replacement"), &path).unwrap();
        let roles = cache.refresh(temp.path(), "123", "BINANCE").unwrap();
        assert_eq!(roles.len(), 1);
        assert!(roles.contains_key("new"));
        std::fs::write(&path, b"").unwrap();
        assert!(
            cache
                .refresh(temp.path(), "123", "BINANCE")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn requires_exact_identity_and_rejects_conflicting_roles_without_inferring_fees() {
        let maker = parse_role(&row("a", "MAKER", "123"), "123", "BINANCE")
            .unwrap()
            .unwrap();
        // Positive fee with explicit MAKER stays Maker; fee sign is irrelevant.
        let mut execution = maker.execution.clone();
        execution.rest = true;
        execution.quantity = "2".into();
        assert_eq!(maker.validate(&execution).unwrap(), FillLiquidity::Maker);
        execution.timestamp_ms += 1;
        assert!(maker.validate(&execution).is_err());
        let mut roles = BTreeMap::new();
        merge(&mut roles, maker.clone()).unwrap();
        merge(&mut roles, maker).unwrap();
        let taker = parse_role(&row("a", "TAKER", "123"), "123", "BINANCE")
            .unwrap()
            .unwrap();
        assert!(merge(&mut roles, taker).is_err());
        assert!(
            parse_role(&row("a", "UNKNOWN", "123"), "123", "BINANCE")
                .unwrap()
                .is_none()
        );
        assert!(
            parse_role(&row("a", "MAKER", "456"), "123", "BINANCE")
                .unwrap()
                .is_none()
        );
        assert!(
            parse_role(&row("a", "MAKER", "123"), "123", "OKX")
                .unwrap()
                .is_none()
        );
    }
}
