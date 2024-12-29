# Xorion

A multi-path UDP proxy applying XOR obfuscation.

## Features

- Listens on multiple UDP ports (server mode) or a single UDP port (client mode).
- Forwards packets to one or more upstream endpoints, applying XOR obfuscation with a user-specified key.

## Quick Start

1. **Build**:

   ```bash
   cargo build --release
   ```

2. Run as server (listening on ports 10001,10002,10003, forwarding to 192.168.1.1:4567, using XOR key 0x7f8c1a44):

   ```bash
   ./target/release/xorion --server \
     --bind 10001,10002,10003 \
     --upstream 192.168.1.1:4567 \
     --obfuscation-key 0x7f8c1a44
   ```

3. Run as client (listening on port 4567, forwarding to 192.168.1.1:10001,10002,10003, using the same XOR key):

   ```bash
   ./target/release/xorion --client \
     --bind 4567 \
     --upstream 192.168.1.1:10001,10002,10003 \
     --obfuscation-key 0x7f8c1a44
   ```

## Testing

To run unit tests:

```bash
cargo test
```

## License

MIT License. See LICENSE file for details.
