use rocksdb::{DB, Options};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("database path required")?;
    let mut options = Options::default();
    options.create_if_missing(false);
    options.set_max_open_files(128);
    options.create_missing_column_families(false);
    let families = DB::list_cf(&options, &path)?;
    let db = DB::open_cf_for_read_only(&options, &path, families, false)?;
    let cf = db.cf_handle("position_updates").ok_or("position_updates missing")?;
    for name in ["rocksdb.estimate-num-keys", "rocksdb.total-sst-files-size", "rocksdb.estimate-live-data-size"] {
        println!("{}={:?}", name, db.property_int_value_cf(&cf, name)?);
    }
    let mut read_options = rocksdb::ReadOptions::default();
    read_options.fill_cache(false);
    let mut iterator = db.raw_iterator_cf_opt(&cf, read_options);
    iterator.seek_to_first();
    let mut count: u64 = 0;
    while iterator.valid() {
        count += 1;
        if count % 1_000_000 == 0 { println!("counted_keys={count}"); }
        iterator.next();
    }
    iterator.status()?;
    println!("exact_archive_keys={count}");
    Ok(())
}
