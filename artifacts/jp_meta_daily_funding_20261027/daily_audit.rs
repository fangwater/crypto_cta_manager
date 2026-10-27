//! Disposable read-only secondary used to verify the deployed daily fee rows.
use anyhow::{Result, ensure};
use rocksdb::{DB, IteratorMode, Options};
use std::path::PathBuf;

fn main() -> Result<()> {
    let paths = std::env::args()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        paths.len() == 2,
        "expected Manager store and fresh secondary paths"
    );
    let primary = paths[0].canonicalize()?;
    let secondary = paths[1]
        .parent()
        .unwrap()
        .canonicalize()?
        .join(paths[1].file_name().unwrap());
    ensure!(
        !secondary.exists() && !secondary.starts_with(&primary),
        "secondary must be fresh and outside primary"
    );
    let mut options = Options::default();
    options.set_max_open_files(-1);
    ensure!(
        DB::list_cf(&options, &primary)?
            .iter()
            .any(|cf| cf == "funding_daily"),
        "not a daily-funding Manager store"
    );
    let db =
        DB::open_cf_as_secondary(&options, &primary, &secondary, ["default", "funding_daily"])?;
    let cf = db.cf_handle("funding_daily").unwrap();
    let mut rows = Vec::<serde_json::Value>::new();
    for row in db.iterator_cf(cf, IteratorMode::Start) {
        let (_, value) = row?;
        rows.push(serde_json::from_slice(&value)?);
    }
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}
