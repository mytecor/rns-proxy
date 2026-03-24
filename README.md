# rns-proxy

> **Disclaimer:** This project was created for academic and research purposes
> only. It is not production-ready and should not be used in environments where
> stability, security, or reliability are required.

SOCKS5 proxy that tunnels TCP connections over the [Reticulum Network Stack](https://reticulum.network/). Route arbitrary TCP traffic through Reticulum's encrypted, delay-tolerant mesh network using the standard SOCKS5 protocol.

## How it works

```mermaid
sequenceDiagram
    participant App
    participant Client as Client (SOCKS5 :1080)
    participant RNS as Reticulum Network
    participant Server as Server (exit node)
    participant Target as Target Host

    App->>Client: TCP CONNECT (SOCKS5)
    Client->>Client: SOCKS5 handshake + allocate session_id

    Client->>RNS: Frame(CONNECT, sid, host:port)
    RNS->>Server: Frame(CONNECT, sid, host:port)
    Server->>Target: TcpStream::connect(host:port)
    Target-->>Server: connected
    Server->>RNS: Frame(CONN_OK, sid)
    RNS->>Client: Frame(CONN_OK, sid)
    Client->>App: SOCKS5 success reply

    loop Bidirectional relay
        App->>Client: TCP data
        Client->>RNS: Frame(DATA, sid, payload)
        RNS->>Server: Frame(DATA, sid, payload)
        Server->>Target: TCP write

        Target-->>Server: TCP data
        Server->>RNS: Frame(DATA, sid, payload)
        RNS-->>Client: Frame(DATA, sid, payload)
        Client-->>App: TCP write
    end

    Note over Client,Server: Frame(CLOSE, sid) ends the session
```

The project consists of two components:

- **Server** (exit node) -- registers on the RNS network, accepts incoming links, and proxies TCP connections to target hosts
- **Client** (local proxy) -- runs a local SOCKS5 server, multiplexes all connections through a single encrypted RNS link to the server

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
# or with a custom identity file:
rns-proxy server --identity-file /path/to/identity
```

On first run the server generates a new identity and saves it to `~/.reticulum/rns_proxy_identity`. On subsequent runs it loads the same identity, so the destination hash stays the same.

```
Server started. Client address:
  <32-hex-char-destination-hash>
```

### Start the client

On the local machine:

```bash
rns-proxy client -d <hash-from-server>
```

The client starts a SOCKS5 proxy:

```
SOCKS5 ready: 127.0.0.1:1080
```

To make the proxy accessible from other devices on the network:

```bash
rns-proxy client -d <hash-from-server> -l 0.0.0.0:1080
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

**Server options:**

```
rns-proxy server [--identity-file <PATH>]
```

| Option | Default | Description |
|---|---|---|
| `--identity-file` | `~/.reticulum/rns_proxy_identity` | Path to the persistent identity file |

**Client options:**

```
rns-proxy client -d <HASH> [-l 127.0.0.1:1080]
```

| Option | Default | Description |
|---|---|---|
| `-d`, `--destination` | required | RNS destination hash (hex) |
| `-l`, `--listen` | `127.0.0.1:1080` | Local SOCKS5 listen address |

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
