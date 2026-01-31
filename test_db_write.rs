// Quick test to see if we can write to the database
use std::sync::Arc;

fn main() {
    let db_path = "/root/.timecoin/testnet/db/blocks";
    
    println!("Testing database write at: {}", db_path);
    
    let db = match sled::open(db_path) {
        Ok(d) => {
            println!("✅ Database opened successfully");
            Arc::new(d)
        }
        Err(e) => {
            println!("❌ Failed to open database: {:?}", e);
            return;
        }
    };
    
    // Try to insert a test key
    let test_key = b"test_write_check";
    let test_value = b"test_value_123";
    
    match db.insert(test_key, test_value.as_ref()) {
        Ok(_) => {
            println!("✅ Test write successful");
            
            // Try to read it back
            match db.get(test_key) {
                Ok(Some(v)) => {
                    if v.as_ref() == test_value {
                        println!("✅ Test read successful - value matches");
                    } else {
                        println!("❌ Test read failed - value mismatch");
                    }
                }
                Ok(None) => println!("❌ Test read failed - key not found"),
                Err(e) => println!("❌ Test read failed: {:?}", e),
            }
            
            // Clean up
            let _ = db.remove(test_key);
        }
        Err(e) => {
            println!("❌ Test write failed: {:?}", e);
            println!("   Error type: {:?}", e);
            println!("   Database is likely corrupted or out of disk space");
        }
    }
    
    // Try to flush
    match db.flush() {
        Ok(_) => println!("✅ Flush successful"),
        Err(e) => println!("❌ Flush failed: {:?}", e),
    }
}
