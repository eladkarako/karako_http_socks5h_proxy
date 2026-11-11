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

by design limitations: hostname is hard-coded by design to be `127.0.0.1`. both `SOCKS5h` upstream server and this program must run on same machine.  

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
- CONNECT support: HTTPS tunneling over HTTP/1.1 and HTTP/2, with a 5-minute idle timeout
- Streaming: request and response bodies are forwarded as they arrive, never buffered whole
- SOCKS5h compliant: Uses domain names instead of IP addresses (privacy-friendly)
- Optimized: LTO compilation, socket tuning, graceful shutdown
- Cross-platform: Windows icons/manifests, Linux mimalloc, tested on x86_64/ARM

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

