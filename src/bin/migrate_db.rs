use timed::block::types::{Block, BlockV1};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        println!("Usage: cargo run --bin migrate_db -- <db_path> <start_block> [end_block]");
        println!(
            "Example: cargo run --bin migrate_db -- ~/.timecoin/mainnet/blockchain_storage 1 50"
        );
        return Ok(());
    }

    let db_path = &args[1];
    let start: u64 = args[2].parse()?;
    let end: u64 = if args.len() >= 4 {
        args[3].parse()?
    } else {
        start
    };

    println!("Opening database: {}", db_path);
    let db = sled::open(db_path)?;

    println!("Migrating blocks {} to {}", start, end);
    println!();

    let mut migrated = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for height in start..=end {
        let old_key = format!("block:{}", height);
        let new_key = format!("block_{}", height);

        // Check if already migrated
        if db.contains_key(new_key.as_bytes())? {
            println!("Block {} already migrated (new key exists)", height);
            skipped += 1;
            continue;
        }

        // Try to read with old key
        let data = match db.get(old_key.as_bytes())? {
            Some(d) => d,
            None => {
                println!("Block {} not found with old key", height);
                failed += 1;
                continue;
            }
        };

        println!("Block {}: Found {} bytes with old key", height, data.len());

        // Try to deserialize as BlockV1
        let block_v1: BlockV1 = match bincode::deserialize(&data) {
            Ok(b) => b,
            Err(e) => {
                println!("  ✗ Failed to deserialize as BlockV1: {}", e);

                // Try as current Block format
                match bincode::deserialize::<Block>(&data) {
                    Ok(_) => {
                        println!("  ✓ Deserializes as current Block format");
                        // Just copy to new key
                        db.insert(new_key.as_bytes(), data.as_ref())?;
                        println!("  ✓ Copied to new key");
                        migrated += 1;
                        continue;
                    }
                    Err(e2) => {
                        println!("  ✗ Failed to deserialize as Block: {}", e2);
                        failed += 1;
                        continue;
                    }
                }
            }
        };

        println!("  ✓ Deserialized as BlockV1");
        println!("    Height: {}", block_v1.header.height);
        println!("    Leader: {}", block_v1.header.leader);
        println!("    Transactions: {}", block_v1.transactions.len());

        // Convert to new format
        let block: Block = block_v1.into();

        // Serialize new format
        let new_data = bincode::serialize(&block)?;
        println!(
            "  ✓ Serialized as Block ({} bytes -> {} bytes)",
            data.len(),
            new_data.len()
        );

        // Write with new key
        db.insert(new_key.as_bytes(), new_data)?;
        println!("  ✓ Migrated to new key: {}", new_key);

        migrated += 1;
    }

    // Flush to disk
    db.flush()?;

    println!();
    println!("Migration complete:");
    println!("  Migrated: {}", migrated);
    println!("  Skipped: {}", skipped);
    println!("  Failed: {}", failed);

    Ok(())
}
