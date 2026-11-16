//! Synthetic isolated fixture for testing the real public SSH pull transport.
use super::*;
use crate::manager_db::{ManagerDb, POSITION_UPDATES_CF};
use crate::order_config::TargetPosition;
use crate::position_archive::{PositionArchive, published_account};
use crate::strategy_catalog::PositionStrategy;
use std::collections::BTreeMap;

pub fn prepare(root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.is_absolute() && !root.exists(),
        "use a new absolute isolated fixture root"
    );
    private_dir(root)?;
    durable_json(&root.join("SYNTHETIC_COLD_REHEARSAL.json"), &true)?;
    let db = ManagerDb::open(&root.join("hot"))?;
    let now = crate::kline::now_us();
    let cf = db.db().cf_handle(POSITION_UPDATES_CF).unwrap();
    for (n, ts) in (0..32)
        .map(|n| (n, now - 40 * 86_400_000_000 + n))
        .chain([(32, now - 86_400_000_000)])
    {
        let strategy = PositionStrategy {
            strategy_name: "synthetic-public-pull".into(),
            updated_at_us: ts,
            targets: BTreeMap::from([(
                "BTCUSDT".into(),
                TargetPosition::new(n as f64 / 17., 1).map_err(anyhow::Error::msg)?,
            )]),
            symbol_order_strategy_overrides: BTreeMap::new(),
        };
        let msg = PositionUpdateMsg::from_strategy(
            ts,
            0,
            strategy,
            vec![],
            vec![published_account(
                "synthetic-source",
                "synthetic-public-pull",
                0.7,
            )],
        );
        db.db()
            .put_cf(&cf, encode_seq_key(ts, 0)?, serde_json::to_vec(&msg)?)?;
    }
    drop(cf);
    let archive = PositionArchive::open_with_cold(
        db,
        Some(manager::Settings {
            root: root.join("cold"),
            ..Default::default()
        }),
    )?;
    let segment = archive
        .archive_cold_once(now)?
        .context("synthetic archive was empty")?;
    ensure!(
        segment.count == 32 && archive.scan_from(now - 86_400_000_000)?.len() == 1,
        "synthetic retained-window mismatch"
    );
    let proof = serde_json::json!({"synthetic_only":true,"source_identity":segment.source_identity,"cold_root":root.join("cold"),"segment":segment,"retained_hot_records":1});
    durable_json(&root.join("proof.json"), &proof)?;
    Ok(proof)
}
