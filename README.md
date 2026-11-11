# `karako_http_socks5h_proxy`

<hr/>

```txt
HTTP proxy that tunnels through SOCKS5h

Usage: karako_http_socks5h_proxy [OPTIONS]

Options:
  -p, --http-port <HTTP_PORT>          [default: 8765]
  -u, --upstream-port <UPSTREAM_PORT>  [default: 5678]
  -b, --bind <BIND>                    Address to bind the HTTP proxy listener on [default: 127.0.0.1]
      --upstream-host <UPSTREAM_HOST>  Host of the upstream SOCKS5 server [default: 127.0.0.1]
      --socks5-username <SOCKS5_USERNAME>  Username for SOCKS5 username/password authentication
      --socks5-password <SOCKS5_PASSWORD>  Password for SOCKS5 username/password authentication
  -v, --verbose...                     Increase log verbosity (use twice to dump SOCKS5 packets)
  -q, --quiet                          Only log errors
  -h, --help                           Print help
  -V, --version                        Print version
```

<hr/>

gives your programs a way to use an upstream `SOCKS5h` server,  
including its built-in DNS resolution on remote. meaning your own machine's DNS/HOSTS will not be used.

you can use it with aria2c, yt-dlp, ffmpeg, wget, curl (although it has `SOCKS5h` support),  
native batch-files scripts with `HTTPS_PROXY` and `HTTP_PROXY`. 

<hr/>

<details><summary>more</summary>

most programs will use HTTP1.1 `CONNECT`, and once the TCP tunnel established it supports any traffic  
that the `SOCKS5h` upstream server would support (HTTP, HTTPS, ...).  
you can set firefox and edge browsers to use it as HTTP proxy, and it works very well.

by default the upstream `SOCKS5h` server is expected on `127.0.0.1`; use `--upstream-host` if it runs elsewhere.  
the HTTP listener binds to `127.0.0.1` by default; use `--bind` for another address such as `::1` or `0.0.0.0`.  
both the proxy and the upstream server normally run on the same machine, but they no longer have to.

binaries provided, supports many OS and architectures.


<hr/>

logging: by default the proxy writes one line per request and for each CONNECT tunnel.  
add `-v` for handshake, socket and timing details, `-vv` to also dump the raw SOCKS5 packets, or `-q` to only log errors.  

| Flag          | Level    | What you see                                                                                                              |
|---------------|----------|---------------------------------------------------------------------------------------------------------------------------|
| (default)     | `info`   | startup, one line per request, CONNECT handshake/tunnel summary, bytes transferred.                                       |
| `-v`          | `debug`  | socket tuning, upstream connect/handshake times, CONNECT packet metadata, response status + elapsed, accept/close events. |
| `-vv`         | `trace`  | everything above plus raw SOCKS5 packet hex dumps (greeting, CONNECT, bind address).                                      |
| `-q`          | `error`  | only failures.                                                                                                            |

sample `-vv`

```
[SOCKS5] CONNECT example.com:443 (atyp=0x03, 18 bytes)
[SOCKS5] CONNECT out (18 bytes):
05 01 00 03 0B 65 78 61 6D 70 6C 65 2E 63 6F 6D
01 BB
[UPSTREAM] -> 200 OK in 647ms (12 response header(s))
[HTTP/1.1] GET http://example.com/ -> 200 OK in 2.48s
[TUNNEL] Tunnel example.com:443 closed normally after 707ms: client→target: 1893 bytes, target→client: 6006 bytes
```

<hr/>

## How to use / manual test recipes

The examples below assume a SOCKS5h server on
`127.0.0.1:9150` and start the proxy on port `18765`:

```bash
karako_http_socks5h_proxy -p 18765 -u 9150
```

### Plain HTTP through the proxy

```bash
curl -x http://127.0.0.1:18765 http://example.com/
```

### HTTPS through a CONNECT tunnel

```bash
curl -k -x http://127.0.0.1:18765 https://example.com/
```

`-k` is only needed here because the curl build may not trust the test
certificate chain; HTTPS itself is tunnelled end to end.

### HEAD, 204 and 304 responses

```bash
curl -I -x http://127.0.0.1:18765 http://example.com/
curl -s -o NUL -w "%{http_code}\n" -x http://127.0.0.1:18765 http://httpbin.org/status/204
curl -s -o NUL -w "%{http_code}\n" -x http://127.0.0.1:18765 http://httpbin.org/status/304
```

### Streaming a large request body

```bash
# 1 MiB of random data
python -c "import os;open('body.bin','wb').write(os.urandom(1024*1024))"
curl -x http://127.0.0.1:18765 --data-binary @body.bin http://httpbin.org/post
```

The proxy forwards the body as it arrives; it is never collected in
memory.

### Verify upstream connection reuse

Send two requests to the same origin in one command. With `-v` you
should see a single `SOCKS5 Handshake complete` line for that origin:

```bash
karako_http_socks5h_proxy -p 18765 -u 9150 -v
curl -s -o NUL -x http://127.0.0.1:18765 http://example.com/ http://example.com/
```

### Inspect the `Via` header

```bash
curl -s -D - -o NUL -x http://127.0.0.1:18765 http://example.com/ | findstr /i "^via:"
```

### Listen on IPv6 loopback

```bash
karako_http_socks5h_proxy -p 18765 -u 9150 --bind ::1
curl -x "http://[::1]:18765" http://example.com/
```

### Upstream SOCKS5 on another host

```bash
karako_http_socks5h_proxy -p 18765 -u 9150 --upstream-host 192.168.1.10
```

### Username/password authentication (RFC 1929)

```bash
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --socks5-username alice --socks5-password secret
```

some proxies accepts any username/password and uses the username as an isolation
key; other SOCKS5 servers may validate them.

### Logging levels

```bash
karako_http_socks5h_proxy -p 18765 -u 9150        # one line per request
karako_http_socks5h_proxy -p 18765 -u 9150 -v     # handshake, socket and timing details
karako_http_socks5h_proxy -p 18765 -u 9150 -vv    # also dump raw SOCKS5 packets
karako_http_socks5h_proxy -p 18765 -u 9150 -q     # errors only
```

</details>

<hr/>

<a href="https://paypal.me/31adkarak0" target="_blank" rel="noopener noreferrer">
  <img src="resources/logos/paypal1.svg" alt="Donate via PayPal">
  <br />
  <img src="resources/logos/paypal2.png" alt="PayPal Donation">
</a>


<hr/>
<hr/>

Key features:
- Protocol detection: HTTP/1.1 and HTTP/2 handled by `hyper-util`'s auto server
- Upstream pooling: HTTP connections are reused per `host:port` through the SOCKS5h server
- CONNECT support: HTTPS tunneling over HTTP/1.1 and HTTP/2, with a 5-minute idle timeout
- Streaming: request and response bodies are forwarded as they arrive, never buffered whole, with a 60-second upstream idle timeout
- SOCKS5h compliant: Uses domain names instead of IP addresses (privacy-friendly)
- SOCKS5 authentication: optional RFC 1929 username/password support
- Robustness: bounded concurrency, graceful drain on shutdown, task panics are contained and logged
- Optimized: LTO compilation, socket tuning, `release-size` profile
- Cross-platform: Windows icons/manifests and console-close handling, Linux mimalloc, tested on x86_64/ARM

<hr/>

The following diagrams cover:
1. Architecture — Component relationships
2. HTTP/1.1 regular requests — Normal proxying
3. HTTP/1.1 CONNECT — HTTPS tunneling
4. SOCKS5h handshake — Protocol exchange with domain-based routing
5. Protocol detection — How HTTP/1.1 vs HTTP/2 is identified
6. Socket configuration — When different config is applied
7. Error handling — Error paths through the system
8. Port setup — How to configure the proxy

<details><summary>diagrams</summary>

<hr/>

```txt
================================================================================
                    ARCHITECTURE OVERVIEW
================================================================================

    HTTP/1.1 + HTTP/2 Client
              |
              | TCP connection
              ↓
    ┌─────────────────────────┐
    │  Protocol Detection     │
    │  (hyper-util auto)      │
    └──────────┬──────────────┘
               |
        ┌──────┴──────┐
        |             |
    HTTP/1.1      HTTP/2
        |             |
        ↓             ↓
    ┌────────┐   ┌────────┐
    │HTTP/1.1│   │HTTP/2  │
    │Handler │   │Handler │
    └────┬───┘   └────┬───┘
         |            |
         └──────┬─────┘
                |
         ┌──────▼──────────────────┐
         │  Request Processing     │
         │ • Extract host:port     │
         │ • Stream body           │
         │ • Build forward request │
         └──────┬──────────────────┘
                |
         ┌──────▼──────────────────┐
         │ SOCKS5h Connection      │
         │ • Handshake (10s)       │
         │ • Domain-based routing  │
         │ • DNS on upstream       │
         └──────┬──────────────────┘
                |
         ┌──────▼──────────────────┐
         │  Upstream Target        │
         │  127.0.0.1:5678         │
         │  (SOCKS5h Server)       │
         └─────────────────────────┘
```

<hr/>

```txt
================================================================================
                 HTTP/1.1 REQUEST FLOW (Regular)
================================================================================

Client                         Proxy                    Upstream Target
  |                              |                              |
  | GET /path HTTP/1.1            |                              |
  | Host: example.com             |                              |
  |─────────────────────────────>|                              |
  |                               | 1. Extract host:port        |
  |                               |    (example.com:80)         |
  |                               |                              |
  |                               | 2. Connect to SOCKS5        |
  |                               |────────────────────────────>|
  |                               |<────────────SOCKS5 OK────────|
  |                               |                              |
  |                               | 3. Forward HTTP request     |
  |                               |    to example.com:80        |
  |                               |────────────────────────────>|
  |                               |                              |
  |                               | 4. Receive HTTP response    |
  |                               |<────────────────────────────|
  |          HTTP Response        |                              |
  |<─────────────────────────────|                              |
  |                               |                              |

```

<hr/>

```txt
================================================================================
              HTTP/1.1 CONNECT TUNNEL FLOW (HTTPS)
================================================================================

Client                         Proxy                    Upstream Target
  |                              |                              |
  | CONNECT example.com:443       |                              |
  |─────────────────────────────>|                              |
  |                               | 1. Extract host:port        |
  |                               |    (example.com:443)        |
  |                               |                              |
  |                               | 2. Connect to SOCKS5        |
  |                               |────────────────────────────>|
  |                               |<────────────SOCKS5 OK────────|
  |                               |                              |
  |        200 Connection         |                              |
  |<─────────────────────────────|                              |
  |                               | 3. Upgrade & tunnel         |
  |◄─── HTTPS tunnel active ─────────────────────────────────►|
  |                               |                              |
  | CLIENT_HELLO (TLS)            |                              |
  |─────────────────────────────>|────────────────────────────>|
  |                               |                              |
  |          SERVER_HELLO         |                              |
  |<─────────────────────────────|<────────────────────────────|
  |                               |                              |
  |  [bidirectional HTTPS copy]   |                              |
  |◄──────────────────────────────────────────────────────────►|
  |                               | (5-minute idle timeout)      |
  |                               |                              |
  |        [connection close]     |                              |
  |─────────────────────────────>|────────────────────────────>|
  |                               |                              |

```

<hr/>

```txt
================================================================================
           SOCKS5h HANDSHAKE SEQUENCE (Domain Resolution)
================================================================================

HTTP Proxy (Client)            SOCKS5h Server (Upstream)
       |                                  |
       | GREETING: [VER=5, NMETHODS=1,  |
       | METHOD=0 (no auth)]             |
       |──────────────────────────────>|
       |                                  |
       |          METHOD SELECTION:      |
       |          [VER=5, METHOD=0]      |
       |<──────────────────────────────|
       |                                  |
       | CONNECT REQUEST:                |
       | [VER=5, CMD=1, RSV=0,           |
       |  ATYP=3 (domain),               |
       |  DLEN, DNAME..., PORT]          |
       |──────────────────────────────>|
       |                                  |
       |   (DNS lookup on server)        |
       |   (Connect to target)           |
       |                                  |
       |       CONNECT RESPONSE:         |
       |       [VER=5, REP=0, RSV=0,    |
       |        ATYP, BIND_ADDR, PORT]  |
       |<──────────────────────────────|
       |                                  |
       | ✓ Tunnel established            |
       | Ready for HTTP request          |
       |                                  |

```

<hr/>

```txt
================================================================================
           PROTOCOL DETECTION (hyper-util auto server)
================================================================================

Incoming bytes                    Detected protocol     Handler
──────────────────────────────────────────────────────────────────
"PRI * HTTP/2.0" preface     ──>  HTTP/2            ──>  HTTP/2 Service
                                (RFC 9113)

Any HTTP/1.x request line    ──>  HTTP/1.1          ──>  HTTP/1.1 Service
(GET, POST, CONNECT, ...)

TLS ClientHello / other      ──>  Not HTTP          ──>  Connection closed
binary garbage
```

<hr/>

```txt
================================================================================
              SOCKET CONFIGURATION STAGES
================================================================================

Connection Lifecycle        Socket Config Applied
──────────────────────────────────────────────────
Inbound (client accept)  ──>  CLIENT_INBOUND
                              • TCP_NODELAY
                              • Socket tuning

SOCKS5 handshake        ──>  SOCKS_HANDSHAKE
                              • 10s timeout
                              • Handshake-specific opts

HTTP request forward    ──>  SOCKS_HTTP_REQUEST
                              • Request-specific opts

CONNECT tunnel          ──>  SOCKS_TUNNEL
                              • Tunnel-specific opts
                              • 5-minute idle timeout
```

<hr/>

```txt
================================================================================
                    ERROR HANDLING FLOW
================================================================================

Request Processing
       |
       ├─> Malformed Request / Missing Host
       |   └─> 400 Bad Request
       |
       ├─> Host/Port Extraction Error
       |   └─> 400 Bad Request
       |
       ├─> SOCKS5 Connection Error (10s timeout)
       |   ├─> Connection refused
       |   └─> 502 Bad Gateway
       |
       ├─> SOCKS5 Handshake Error
       |   ├─> Invalid version
       |   ├─> Method rejected (0xFF)
       |   ├─> SOCKS5 reply codes (0-8 mapped to error messages)
       |   └─> 502 Bad Gateway
       |
       ├─> HTTP Request Forward Error
       |   └─> 502 Bad Gateway
       |
       └─> Tunnel idle for 5 minutes
           └─> Connection close + warning log
```

<hr/>

```txt
================================================================================
              PORT CONFIGURATION EXAMPLE
================================================================================

Your HTTP Proxy                SOCKS5h Server
┌──────────────────┐          ┌──────────────┐
│  127.0.0.1:8765  │◄────────►│127.0.0.1:5678│
│  (HTTP clients   │          │ (Upstream    │
│   connect here)  │          │  tunnel to   │
└──────────────────┘          │  real SOCKS) │
                              └──────────────┘

Usage:
  cargo run -- --http-port 8765 --upstream-port 5678

Browser settings:
  HTTP Proxy:   127.0.0.1:8765
  HTTPS Proxy:  127.0.0.1:8765 (CONNECT tunneling)
```

</details>

<hr/>

<br/>
<hr/>
<br/>

