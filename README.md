# `karako_http_socks5h_proxy`

<hr/>

```txt
HTTP proxy that tunnels through SOCKS5h

Usage: karako_http_socks5h_proxy [OPTIONS]

Options:
  -p, --http-port <HTTP_PORT>          [default: 8765]
  -u, --upstream-port <UPSTREAM_PORT>  [default: 5678]
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

known issues/bugs: in `HEAD` (and probably `HTTP 204` and `HTTP 304`) there is zero length body which trigger some quirk in `hyper`. I'm trying to work around it.  

binaries provided, supports many OS and architectures.

still under development so a lot of debug information is written..

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
- Protocol detection: Automatically detects HTTP/1.1 vs HTTP/2
- CONNECT support: Full HTTPS tunneling with 10-minute timeouts
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
    │  (peek first 24 bytes)  │
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
         │ • Read body             │
         │ • Build forward request │
         └──────┬──────────────────┘
                |
         ┌──────▼──────────────────┐
         │ SOCKS5h Connection      │
         │ • Handshake (30s)       │
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
  |                               | (10-minute timeout)          |
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
           PROTOCOL DETECTION (First 24 Bytes)
================================================================================

Received Data              Pattern Match       Handler
──────────────────────────────────────────────────────────
"PRI * HTTP/2.0"   ──>   HTTP/2 Preface   ──>  HTTP/2 Service
                         (RFC 9113)

"GET / HTTP/1.1"   ──>   HTTP/1.1 Method  ──>  HTTP/1.1 Service
"POST / HTTP/1.1"        + CRLF pattern
"CONNECT : HTTP/1.1"

"OPTIONS / HTTP/1.1"
"DELETE / HTTP/1.1"

[Other data]       ──>   Unknown           ──>  Error: Protocol not supported
                         (< 4 bytes)       ──>  Error: Connection closed
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
                              • 30s timeout
                              • Handshake-specific opts

HTTP request forward    ──>  SOCKS_HTTP_REQUEST
                              • Request-specific opts

CONNECT tunnel          ──>  SOCKS_TUNNEL
                              • Tunnel-specific opts
                              • 10-minute timeout
```

<hr/>

```txt
================================================================================
                    ERROR HANDLING FLOW
================================================================================

Request Processing
       |
       ├─> Protocol Detection Error
       |   └─> 500 / Connection Close
       |
       ├─> Host/Port Extraction Error
       |   └─> 400 Bad Gateway
       |
       ├─> SOCKS5 Connection Error (30s timeout)
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
       └─> Tunnel Timeout (10 minutes)
           └─> Connection close + Error log
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

