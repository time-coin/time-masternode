#!/usr/bin/env python3
"""
Migration script for Timecoin blocks with schema changes
Reads old block format and re-serializes with new format
"""

import struct
import sys
import os
import bincode  # pip install bincode

def read_u32_le(data, pos):
    return struct.unpack('<I', data[pos:pos+4])[0], pos + 4

def read_u64_le(data, pos):
    return struct.unpack('<Q', data[pos:pos+8])[0], pos + 8

def read_i64_le(data, pos):
    return struct.unpack('<q', data[pos:pos+8])[0], pos + 8

def read_u128_le(data, pos):
    return int.from_bytes(data[pos:pos+16], 'little'), pos + 16

def read_hash256(data, pos):
    return data[pos:pos+32], pos + 32

def read_string(data, pos):
    """Read bincode string (length-prefixed)"""
    length, pos = read_u64_le(data, pos)
    return data[pos:pos+length].decode('utf-8'), pos + length

def read_vec_u8(data, pos):
    """Read Vec<u8>"""
    length, pos = read_u64_le(data, pos)
    return data[pos:pos+length], pos + length

def write_u32_le(value):
    return struct.pack('<I', value)

def write_u64_le(value):
    return struct.pack('<Q', value)

def write_i64_le(value):
    return struct.pack('<q', value)

def write_u128_le(value):
    return value.to_bytes(16, 'little')

def write_hash256(value):
    return value

def write_string(s):
    """Write bincode string"""
    encoded = s.encode('utf-8')
    return write_u64_le(len(encoded)) + encoded

def write_vec_u8(data):
    """Write Vec<u8>"""
    return write_u64_le(len(data)) + data

def write_option_vec_u8(data):
    """Write Option<Vec<u8>>"""
    if data is None or len(data) == 0:
        return b'\x00'  # None
    else:
        return b'\x01' + write_vec_u8(data)  # Some(vec)

def write_option_bool(value):
    """Write Option<bool>"""
    if value is None:
        return b'\x00'  # None
    else:
        return b'\x01' + (b'\x01' if value else b'\x00')  # Some(bool)

def parse_old_block_header(data, pos):
    """Parse old BlockHeaderV1 format"""
    print(f"Parsing block header from position {pos}")
    
    # version: u32
    version, pos = read_u32_le(data, pos)
    print(f"  version: {version}")
    
    # height: u64
    height, pos = read_u64_le(data, pos)
    print(f"  height: {height}")
    
    # timestamp: i64
    timestamp, pos = read_i64_le(data, pos)
    print(f"  timestamp: {timestamp}")
    
    # previous_hash: [u8; 32]
    previous_hash, pos = read_hash256(data, pos)
    print(f"  previous_hash: {previous_hash[:8].hex()}...")
    
    # merkle_root: [u8; 32]
    merkle_root, pos = read_hash256(data, pos)
    print(f"  merkle_root: {merkle_root[:8].hex()}...")
    
    # leader: String
    leader, pos = read_string(data, pos)
    print(f"  leader: {leader}")
    
    # block_reward: u64
    block_reward, pos = read_u64_le(data, pos)
    print(f"  block_reward: {block_reward}")
    
    # vrf_output: [u8; 32]
    vrf_output, pos = read_hash256(data, pos)
    print(f"  vrf_output: {vrf_output[:8].hex()}...")
    
    # vrf_score: u128
    vrf_score, pos = read_u128_le(data, pos)
    print(f"  vrf_score: {vrf_score}")
    
    # attestation_root: [u8; 32]
    attestation_root, pos = read_hash256(data, pos)
    print(f"  attestation_root: {attestation_root[:8].hex()}...")
    
    return {
        'version': version,
        'height': height,
        'timestamp': timestamp,
        'previous_hash': previous_hash,
        'merkle_root': merkle_root,
        'leader': leader,
        'block_reward': block_reward,
        'vrf_output': vrf_output,
        'vrf_score': vrf_score,
        'attestation_root': attestation_root,
    }, pos

def serialize_new_block_header(header):
    """Serialize BlockHeader with new fields"""
    data = b''
    
    # Same fields as before
    data += write_u32_le(header['version'])
    data += write_u64_le(header['height'])
    data += write_i64_le(header['timestamp'])
    data += write_hash256(header['previous_hash'])
    data += write_hash256(header['merkle_root'])
    data += write_string(header['leader'])
    data += write_u64_le(header['block_reward'])
    data += write_hash256(header['vrf_output'])
    data += write_u128_le(header['vrf_score'])
    data += write_hash256(header['attestation_root'])
    
    # NEW FIELDS (with defaults)
    # masternode_tiers: MasternodeTierCounts (4 x u32 = 16 bytes)
    data += write_u32_le(0)  # free
    data += write_u32_le(0)  # bronze
    data += write_u32_le(0)  # silver
    data += write_u32_le(0)  # gold
    
    # vrf_proof: Vec<u8>
    data += write_vec_u8(b'')  # empty
    
    # active_masternodes_bitmap: Vec<u8>
    data += write_vec_u8(b'')  # empty
    
    # liveness_recovery: Option<bool>
    data += write_option_bool(None)
    
    return data

def print_usage():
    print("Usage:")
    print("  python migrate_blocks.py <database_path> inspect <block_num>")
    print("  python migrate_blocks.py <database_path> migrate <start> <end>")
    print()
    print("Examples:")
    print("  python migrate_blocks.py ~/.timecoin/mainnet/blockchain_storage inspect 1")
    print("  python migrate_blocks.py ~/.timecoin/mainnet/blockchain_storage migrate 1 50")

if __name__ == "__main__":
    if len(sys.argv) < 3:
        print_usage()
        sys.exit(1)
    
    db_path = os.path.expanduser(sys.argv[1])
    command = sys.argv[2]
    
    if command == "inspect":
        if len(sys.argv) < 4:
            print("ERROR: Block number required")
            print_usage()
            sys.exit(1)
        
        block_num = int(sys.argv[3])
        print(f"Inspecting block {block_num} in {db_path}")
        print()
        
        # Read database
        db_file = os.path.join(db_path, "db")
        if not os.path.exists(db_file):
            print(f"ERROR: Database file not found: {db_file}")
            sys.exit(1)
        
        with open(db_file, 'rb') as f:
            db_data = f.read()
        
        # Find block with old key format
        old_key = f"block:{block_num}".encode()
        pos = db_data.find(old_key)
        
        if pos == -1:
            print(f"Block {block_num} not found with old key format")
            sys.exit(1)
        
        print(f"Found at offset {pos}")
        print(f"Key: {old_key.decode()}")
        print()
        
        # Parse block data (starts after key)
        # Sled format is complex, but typically the value follows the key
        # Try various offsets to find where the actual block data starts
        for offset in range(20):
            try:
                print(f"\n{'='*70}")
                print(f"Trying offset {offset} bytes after key:")
                print(f"{'='*70}")
                data_start = pos + len(old_key) + offset
                header, _ = parse_old_block_header(db_data, data_start)
                
                if header['height'] == block_num:
                    print(f"\n✓ Successfully parsed block {block_num}!")
                    print(f"  Correct offset: {offset} bytes after key")
                    break
            except Exception as e:
                print(f"  Failed: {e}")
                continue
    
    elif command == "migrate":
        print("Migration not yet implemented")
        print("Use the Rust-based migration tool instead")
        sys.exit(1)
    
    else:
        print(f"ERROR: Unknown command: {command}")
        print_usage()
        sys.exit(1)
