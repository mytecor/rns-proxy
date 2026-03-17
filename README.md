# rns-proxy

> **Disclaimer:** This project was created for academic and research purposes
> only. It is not production-ready and should not be used in environments where
> stability, security, or reliability are required.

SOCKS5 proxy that tunnels TCP connections over the [Reticulum Network Stack](https://reticulum.network/). Route arbitrary TCP traffic through Reticulum's encrypted, delay-tolerant mesh network using the standard SOCKS5 protocol.

## How it works

```
[App] -> [SOCKS5 Client :1080] --RNS Link--> [Server (exit node)] -> [Target host]
```

The project consists of two components:

- **Server** (exit node) -- registers on the RNS network, accepts incoming links, and proxies TCP connections to target hosts
- **Client** (local proxy) -- runs a SOCKS5 server on `127.0.0.1:1080`, multiplexes all connections through a single encrypted RNS link to the server

All TCP sessions are multiplexed over one RNS link using a custom binary frame protocol. Frames larger than `LINK_MDU` are automatically chunked on send and reassembled on receive.

The client handles automatic reconnection when the link or underlying transport is lost, with exponential backoff and full RNS node recreation after repeated failures.

## Build

### Cargo

```bash
cargo build --release
```

### Nix

```bash
nix build
# or enter dev shell:
nix develop
```

## Usage

### Prerequisites

A running Reticulum daemon (`rnsd`). Install via `pip install rns`.

```bash
rnsd
# or use the included example config:
./examples/run_rnsd.sh
```

### Start the server

On the exit node machine:

```bash
rns-proxy server
```

The server prints its destination hash on startup:

```
Server started. Client address:
  <32-hex-char-destination-hash>
```

### Start the client

On the local machine:

```bash
rns-proxy client --server <hash-from-server>
```

The client starts a SOCKS5 proxy:

```
SOCKS5 ready: 127.0.0.1:1080
```

### Use it

Configure any application to use `127.0.0.1:1080` as a SOCKS5 proxy:

```bash
curl --socks5 127.0.0.1:1080 https://example.com
```

## CLI

```
rns-proxy [OPTIONS] <COMMAND>

Commands:
  server   Run the proxy server (exit node)
  client   Run the proxy client (local SOCKS5)

Options:
  --debug           Enable debug logging
  -V, --version     Print version
  -h, --help        Print help
```

**Client options:**

```
rns-proxy client --server <HASH> [--port 1080]
```

| Option | Default | Description |
|---|---|---|
| `--server` | required | Server destination hash (hex) |
| `--port` | `1080` | Local SOCKS5 listen port |

## Protocol

Multiplexed frame format (wire-compatible with the [Python implementation](https://github.com/rsgrinko/reticulum-socks5-proxy)):

```
[1 byte type][4 bytes session_id][2 bytes payload_len][payload]
```

| Type | Code | Description |
|---|---|---|
| CONNECT | `0x01` | Request connection to host:port |
| CONN_OK | `0x02` | Connection succeeded |
| CONN_ERR | `0x03` | Connection error (payload = UTF-8 reason) |
| DATA | `0x04` | Bidirectional data |
| CLOSE | `0x05` | Close session |

## Compatibility

Wire-compatible with [reticulum-socks5-proxy](https://github.com/rsgrinko/reticulum-socks5-proxy) (Python). Both implementations use the same RNS destination (`rns_socks` / `proxy`), identical frame protocol, and the same multiplexing scheme. You can mix and match -- for example, run the Python server with the Rust client, or vice versa.

## Configuration

- **Logging** -- `--debug` flag or `RUST_LOG` environment variable (`RUST_LOG=trace`)
- **Identity** -- generated fresh on each server start (not persisted)
- **Reticulum** -- uses the default Reticulum config at `~/.reticulum/`

## License

MIT
