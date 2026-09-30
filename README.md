# In one terminal, start your upstream SOCKS5 (Tor):
tor --SocksPort 9050 --UDPSocksPort 9050

# In another, start the proxy:
RUST_LOG=info ./target/release/http-socks5-proxy

# In a third, test it:
curl -x http://127.0.0.1:12345 http://example.com

<hr/>

Key Features Implemented

- TCP→UDP bridge with SOCKS5h protocol
- Hostname forwarding (no local DNS resolution)
- HTTP/1.1 keep-alive support
- Chunked transfer encoding support (via hyper)
- Minimal arguments (--forwarder-listening-port, --upstream-listening-port)
- Battle-tested crates.

<hr/>

# SOCKS5h

SOCKS5h specifically means SOCKS5 with remote hostname resolution — the proxy server resolves the hostname, not the client.

- The HTTP request includes a `Host: example.com` header.
- hostname string is extracted (`"example.com"`) and pass it directly to the SOCKS5 server.
- In `src/socks5.rs`, the UDP packet uses `ATYP=0x03` (domain name type), which tells the SOCKS5 server: "you resolve this hostname, not me"
```rust
// ATYP=0x03 (domain name) — this is SOCKS5h
packet.push(0x03);
packet.push(hostname.len() as u8);
packet.extend_from_slice(hostname.as_bytes());
```

This is SOCKS5h, not plain SOCKS5 (which would use `ATYP=0x01` for IPv4 addresses that the client resolved locally).

The "h" stands for "hostname" — we're sending the raw hostname to the upstream server and letting it resolve it, which is exactly what Tor's UDP SOCKS5 port does.

<hr/>

I've asked Claude Haiku 4.5 and it suggested few practical improvements for this proxy,  
as I still need to test it in few cases those are here as an future-idea,  

most of them are just implementing slimmer functionality instead of using crates,  
organized by impact:

## High-Impact Improvements

### 1. Remove HTTP Parsing Overhead — Use Raw TCP Forwarding
Current issue: We parse HTTP with hyper, extract Host header, rebuild the request. This is wasteful.
Better approach:
- Accept raw TCP connections
- Parse only the first line and Host header (minimal state machine)
- Stream the request body directly through to UDP without buffering
- Stream UDP response directly back without parsing
Memory gain: Eliminates hyper's internal buffering and request/response object allocations.

```rust
// Pseudo: minimal HTTP parser
fn parse_http_header_line(buf: &[u8]) -> Option<(String, u16)> {
    // Single pass, find Host: and extract — no full parsing
}

// Stream request → SOCKS5 UDP → response without intermediate Vec allocations
```

### 2. Eliminate Full Request Buffering — Stream to UDP

Current issue: `hyper::body::to_bytes()` loads the entire body into memory.
Better - For each client TCP connection, spawn a task that: 
- Reads HTTP headers once
- Sends them to SOCKS5 UDP
- Streams body chunks directly to UDP socket (copy-on-write)
- Streams UDP response back to client without buffering

Memory gain: Constant memory per connection regardless of request size (no `Vec` growth).

### 3. UDP Response Buffering — Use Circular Buffer or Ring

Current issue: `let mut response_buf = [0u8; 65535]` allocates `65KB` per request on the stack.

Better:

- Reuse a thread-local or connection-local buffer pool
- Pre-allocate 2–3 buffers per connection, rotate them
- Avoid repeated allocations for repeated requests

```rust
struct BufferPool {
    buffers: Vec<BytesMut>,
    current: usize,
}

impl BufferPool {
    fn get_mut(&mut self) -> &mut BytesMut {
        self.buffers[self.current].clear();
        self.current = (self.current + 1) % self.buffers.len();
        &mut self.buffers[self.current]
    }
}
```

Memory gain: Predictable, bounded allocation.

## Mid-Impact Improvements

### 4. Connection Pooling & Reuse

Current issue: Each HTTP request creates a new service_fn closure and potentially new state.

Better:
- Pool client TCP connections more explicitly
- Reuse the same Socks5Client across all HTTP connections (already done ✓)
- But also: batch small requests into single UDP packets if HTTP pipelining is detected

### 5. Zero-Copy Forwarding with `io_uring` or `splice()`

Current issue: Data copied from client TCP → buffer → UDP → buffer → client TCP.

Better (if using `tokio-uring` or similar):
- Use kernel-level `splice()` or `copy_file_range()` to forward without user-space copies.
- This is overkill for localhost but excellent for high-throughput scenarios

### 6. Adaptive UDP Timeouts
Current issue: Hardcoded `Duration::from_secs(10)` timeout.
Better:
- Track round-trip times per upstream server
- Adjust timeout dynamically (e.g., p95 + margin)
- Retry failed requests once before timing out

```rust
struct RttTracker {
    samples: VecDeque<Duration>,
    max_samples: usize,
}

impl RttTracker {
    fn estimate_timeout(&self) -> Duration {
        let avg = self.samples.iter().sum::<Duration>() / self.samples.len() as u32;
        avg * 3 // p95-ish
    }
}
```

## Low-Impact But Clean

### 7. ATYP Detection Instead of Hardcoding `0x03`

Allow fallback to IPv4 (`ATYP=0x01`) if the hostname is already an IP:

```rust
fn encode_socks5_address(hostname: &str, port: u16) -> Vec<u8> {
    if let Ok(ip) = hostname.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(ipv4) => {
                vec![0x01, ipv4.octets()[0], ipv4.octets()[1], ipv4.octets()[2], ipv4.octets()[3]]
            }
            IpAddr::V6(ipv6) => {
                // ... IPv6 support
            }
        }
    } else {
        // Domain name
        vec![0x03, hostname.len() as u8, hostname.as_bytes()]
    }
}
```

### 8. Lazy Logging with `debug!` Macros

Already in your code, but ensure they're only compiled in debug builds:

```rust
#[cfg(debug_assertions)]
debug!("...");
```

### 9. Connection Draining on Shutdown

Gracefully close in-flight requests:

```rust
let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

tokio::select! {
    _ = server.await => {}
    _ = shutdown_rx.recv() => {
        info!("Shutting down gracefully...");
    }
}
```

<hr/>

## Architecture Redesign for Maximum Efficiency

If you want truly seamless, zero-copy forwarding, consider this redesign:

```txt
Client TCP Connection
    ↓
[Minimal HTTP header parser — extracts Host + Content-Length only]
    ↓
Spawn request task:
  - Send SOCKS5 UDP request (headers only)
  - tokio::io::copy() body directly from client → UDP
  - tokio::io::copy() UDP response directly to client
    ↓
Keep connection open for keep-alive

````

This eliminates:
- Full request/response buffering
- Hyper's overhead
- Intermediate allocations

Trade-off: You lose HTTP-level semantics (can't validate status codes mid-stream), but for a transparent proxy, that's acceptable.

## Memory Profile Comparison

<table>
<thead>
<tr><th> Approach           </th><th> Per-Connection Memory </th><th> Per-Request Buffer                   </th></tr></thead>
<tbody>
<tr><td> Current (hyper)    </td><td> ~100–200 KB           </td><td> 65 KB (response) + request body size </td></tr>
<tr><td> Streaming          </td><td> ~20 KB                </td><td> 8–16 KB (header only)                </td></tr>
<tr><td> With buffer pool   </td><td> ~30 KB                </td><td> 0 KB (reused)                        </td></tr>
<tr><td> io_uring splice    </td><td> ~10 KB                </td><td> Kernel buffers only                  </td></tr>
</tbody>
</table>

Which improvement matters most?  
For high concurrency (1000s of connections), #1 + #2 (raw TCP + streaming) are the biggest wins.  
For large payloads, #3 + #5 (buffer reuse + zero-copy) dominate.

<br/>
<br/>

