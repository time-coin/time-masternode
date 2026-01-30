#!/usr/bin/env python3
"""
Inspect Sled database and decode block data
Helps diagnose deserialization issues
"""

import struct
import sys
import os

def read_varint(data, offset):
    """Read a variable-length integer (bincode format)"""
    if offset >= len(data):
        return None, offset
    
    first_byte = data[offset]
    if first_byte < 251:
        return first_byte, offset + 1
    elif first_byte == 251:
        if offset + 2 >= len(data):
            return None, offset
        return struct.unpack('<H', data[offset+1:offset+3])[0], offset + 3
    elif first_byte == 252:
        if offset + 4 >= len(data):
            return None, offset
        return struct.unpack('<I', data[offset+1:offset+5])[0], offset + 5
    elif first_byte == 253:
        if offset + 8 >= len(data):
            return None, offset
        return struct.unpack('<Q', data[offset+1:offset+9])[0], offset + 9
    return None, offset

def inspect_sled_db(db_path):
    """Inspect sled database files"""
    print(f"Inspecting database: {db_path}\n")
    
    # Check if path exists
    if not os.path.exists(db_path):
        print(f"ERROR: Database path does not exist: {db_path}")
        return
    
    # List files in the database directory
    print("Database files:")
    for f in os.listdir(db_path):
        fpath = os.path.join(db_path, f)
        if os.path.isfile(fpath):
            size = os.path.getsize(fpath)
            print(f"  {f}: {size:,} bytes ({size/1024/1024:.2f} MB)")
    print()
    
    # Try to read the conf file
    conf_file = os.path.join(db_path, "conf")
    if os.path.exists(conf_file):
        print("Database configuration:")
        with open(conf_file, 'rb') as f:
            conf_data = f.read()
            print(f"  Size: {len(conf_data)} bytes")
            print(f"  First 100 bytes (hex): {conf_data[:100].hex()}")
        print()
    
    # Try to read the db file (main data)
    db_file = os.path.join(db_path, "db")
    if os.path.exists(db_file):
        print("Main database file analysis:")
        with open(db_file, 'rb') as f:
            db_data = f.read()
            print(f"  Total size: {len(db_data):,} bytes ({len(db_data)/1024/1024:.2f} MB)")
            
            # Look for block keys
            print("\n  Searching for block keys:")
            old_format_count = 0
            new_format_count = 0
            
            # Search for "block:" (old format)
            pos = 0
            while pos < len(db_data) - 10:
                if db_data[pos:pos+6] == b'block:':
                    # Found old format key
                    key_end = pos + 6
                    while key_end < len(db_data) and chr(db_data[key_end]).isdigit():
                        key_end += 1
                    block_num = db_data[pos+6:key_end].decode('ascii', errors='ignore')
                    if block_num.isdigit():
                        old_format_count += 1
                        if old_format_count <= 5:
                            print(f"    Found old format: block:{block_num} at offset {pos}")
                pos += 1
            
            # Search for "block_" (new format)
            pos = 0
            while pos < len(db_data) - 10:
                if db_data[pos:pos+6] == b'block_':
                    # Found new format key
                    key_end = pos + 6
                    while key_end < len(db_data) and chr(db_data[key_end]).isdigit():
                        key_end += 1
                    block_num = db_data[pos+6:key_end].decode('ascii', errors='ignore')
                    if block_num.isdigit():
                        new_format_count += 1
                        if new_format_count <= 5:
                            print(f"    Found new format: block_{block_num} at offset {pos}")
                pos += 1
            
            print(f"\n  Summary:")
            print(f"    Old format (block:N) keys found: {old_format_count}")
            print(f"    New format (block_N) keys found: {new_format_count}")
            
            # Look for specific blocks
            print("\n  Checking specific blocks (1-10):")
            for block_num in range(1, 11):
                old_key = f"block:{block_num}".encode()
                new_key = f"block_{block_num}".encode()
                
                old_exists = old_key in db_data
                new_exists = new_key in db_data
                
                status = []
                if old_exists:
                    status.append("old_key")
                if new_exists:
                    status.append("new_key")
                
                if status:
                    print(f"    Block {block_num}: {', '.join(status)}")
                else:
                    print(f"    Block {block_num}: NOT FOUND")
    
    print()

def decode_block_header(data, offset=0):
    """Try to decode block header from bincode data"""
    print(f"Attempting to decode block header from offset {offset}...")
    print(f"Data length: {len(data)} bytes")
    print(f"First 200 bytes (hex): {data[offset:offset+200].hex()}\n")
    
    try:
        pos = offset
        
        # Read version (u32)
        if pos + 4 > len(data):
            print("ERROR: Not enough data for version")
            return
        version = struct.unpack('<I', data[pos:pos+4])[0]
        pos += 4
        print(f"Version: {version}")
        
        # Read height (u64)
        if pos + 8 > len(data):
            print("ERROR: Not enough data for height")
            return
        height = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        print(f"Height: {height}")
        
        # Read timestamp (i64)
        if pos + 8 > len(data):
            print("ERROR: Not enough data for timestamp")
            return
        timestamp = struct.unpack('<q', data[pos:pos+8])[0]
        pos += 8
        print(f"Timestamp: {timestamp}")
        
        # Read previous_hash ([u8; 32])
        if pos + 32 > len(data):
            print("ERROR: Not enough data for previous_hash")
            return
        previous_hash = data[pos:pos+32].hex()
        pos += 32
        print(f"Previous hash: {previous_hash[:16]}...")
        
        # Read merkle_root ([u8; 32])
        if pos + 32 > len(data):
            print("ERROR: Not enough data for merkle_root")
            return
        merkle_root = data[pos:pos+32].hex()
        pos += 32
        print(f"Merkle root: {merkle_root[:16]}...")
        
        # Read leader (String)
        leader_len, pos = read_varint(data, pos)
        if leader_len is None or pos + leader_len > len(data):
            print(f"ERROR: Invalid leader string length: {leader_len}")
            return
        leader = data[pos:pos+leader_len].decode('utf-8', errors='replace')
        pos += leader_len
        print(f"Leader: {leader}")
        
        # Read block_reward (u64)
        if pos + 8 > len(data):
            print("ERROR: Not enough data for block_reward")
            return
        block_reward = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        print(f"Block reward: {block_reward}")
        
        # Read vrf_output ([u8; 32])
        if pos + 32 > len(data):
            print("ERROR: Not enough data for vrf_output")
            return
        vrf_output = data[pos:pos+32].hex()
        pos += 32
        print(f"VRF output: {vrf_output[:16]}...")
        
        # Read vrf_score (u128)
        if pos + 16 > len(data):
            print("ERROR: Not enough data for vrf_score")
            return
        vrf_score_bytes = data[pos:pos+16]
        vrf_score = int.from_bytes(vrf_score_bytes, 'little')
        pos += 16
        print(f"VRF score: {vrf_score}")
        
        # Read attestation_root ([u8; 32])
        if pos + 32 > len(data):
            print("ERROR: Not enough data for attestation_root")
            return
        attestation_root = data[pos:pos+32].hex()
        pos += 32
        print(f"Attestation root: {attestation_root[:16]}...")
        
        # Check for optional active_masternodes_bitmap
        if pos < len(data):
            print(f"\nRemaining bytes: {len(data) - pos}")
            print(f"Next bytes (hex): {data[pos:pos+50].hex()}")
            
            # Try to read Option<Vec<u8>> for active_masternodes_bitmap
            option_tag = data[pos] if pos < len(data) else None
            print(f"Option tag: {option_tag}")
            
            if option_tag == 1:  # Some
                pos += 1
                vec_len, pos = read_varint(data, pos)
                print(f"active_masternodes_bitmap length: {vec_len}")
            elif option_tag == 0:  # None
                print("active_masternodes_bitmap: None")
        
        print(f"\nSuccessfully decoded block header!")
        print(f"Bytes consumed: {pos - offset}")
        
    except Exception as e:
        print(f"ERROR decoding: {e}")
        import traceback
        traceback.print_exc()

def extract_block_data(db_path, block_num):
    """Extract raw block data for a specific block number"""
    print(f"Extracting block {block_num} data...\n")
    
    db_file = os.path.join(db_path, "db")
    if not os.path.exists(db_file):
        print(f"ERROR: Database file not found: {db_file}")
        return None
    
    with open(db_file, 'rb') as f:
        db_data = f.read()
    
    # Try both key formats
    old_key = f"block:{block_num}".encode()
    new_key = f"block_{block_num}".encode()
    
    for key_name, key in [("old", old_key), ("new", new_key)]:
        pos = db_data.find(key)
        if pos != -1:
            print(f"Found block {block_num} with {key_name} format at offset {pos}")
            print(f"Key: {key.decode()}")
            
            # Sled stores: key_len | key | value_len | value
            # Skip past the key
            data_start = pos + len(key)
            
            # Try to find the value length (next few bytes after key)
            print(f"\nBytes after key (next 100 bytes):")
            print(db_data[data_start:data_start+100].hex())
            
            # Try to decode the block data starting at various offsets
            for offset in [0, 1, 2, 3, 4, 8]:
                print(f"\n{'='*60}")
                print(f"Trying offset {offset} from key end:")
                decode_block_header(db_data, data_start + offset)
            
            return db_data[data_start:]
    
    print(f"Block {block_num} not found in database")
    return None

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python inspect_db.py <database_path> [block_number]")
        print("Example: python inspect_db.py ~/.timecoin/mainnet/blockchain_storage")
        print("         python inspect_db.py ~/.timecoin/mainnet/blockchain_storage 1")
        sys.exit(1)
    
    db_path = sys.argv[1]
    
    # Expand home directory
    db_path = os.path.expanduser(db_path)
    
    if len(sys.argv) >= 3:
        # Extract specific block
        block_num = int(sys.argv[2])
        extract_block_data(db_path, block_num)
    else:
        # General inspection
        inspect_sled_db(db_path)
