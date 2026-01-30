use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <block_height>", args[0]);
        std::process::exit(1);
    }
    
    let height: u64 = args[1].parse().expect("Invalid height");
    
    // Open the database
    let db = sled::open("/root/.timecoin/testnet/db").expect("Failed to open database");
    
    // Try both key formats
    let key_new = format!("block_{}", height);
    let key_old = format!("block:{}", height);
    
    println!("Checking for block {} with key formats:", height);
    println!("  - New format: {}", key_new);
    println!("  - Old format: {}", key_old);
    println!();
    
    // Try new key format
    if let Ok(Some(data)) = db.get(key_new.as_bytes()) {
        println!("✓ Found block with NEW key format: {}", key_new);
        println!("  Data size: {} bytes", data.len());
        println!("  First 64 bytes (hex): {}", hex::encode(&data[..data.len().min(64)]));
        println!();
        
        // Try to show the structure
        if data.len() > 100 {
            println!("  Data looks complete (>100 bytes)");
        } else {
            println!("  WARNING: Data is very small ({}  bytes) - likely truncated!", data.len());
        }
    } else {
        println!("✗ No block found with NEW key format");
    }
    
    println!();
    
    // Try old key format
    if let Ok(Some(data)) = db.get(key_old.as_bytes()) {
        println!("✓ Found block with OLD key format: {}", key_old);
        println!("  Data size: {} bytes", data.len());
        println!("  First 64 bytes (hex): {}", hex::encode(&data[..data.len().min(64)]));
        println!();
        
        if data.len() > 100 {
            println!("  Data looks complete (>100 bytes)");
        } else {
            println!("  WARNING: Data is very small ({} bytes) - likely truncated!", data.len());
        }
    } else {
        println!("✗ No block found with OLD key format");
    }
}
