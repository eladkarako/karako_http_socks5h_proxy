# `karako_http_socks5h_proxy`

<hr/>

```txt
HTTP-over-SOCKS5h forward proxy.

Listens for HTTP/1.1 and HTTP/2 clients and forwards their requests through an upstream SOCKS5h
server. Host names travel to that server unresolved, so remote DNS is used.

Plain HTTP is forwarded directly; HTTPS and other TLS traffic must use CONNECT.

Example:
  karako_http_socks5h_proxy -p 18765 -u 9150
  curl -x http://127.0.0.1:18765 http://example.com/

Usage: karako_http_socks5h_proxy.exe [OPTIONS]

Options:
  -p, --http-port <HTTP_PORT>
          Port the HTTP proxy listens on for clients
          
          Point clients at `http://<bind>:<http-port>`. This is the port a browser or `curl -x`
          should use, not the SOCKS5 port.
          
          [default: 8765]

  -u, --upstream-port <UPSTREAM_PORT>
          Port of the upstream SOCKS5 server
          
          Usually a local Tor instance (9050, or 9150 for the Tor Browser bundle). The proxy never
          speaks SOCKS5 to clients.
          
          [default: 5678]

  -b, --bind <BIND>
          Address to bind the HTTP proxy listener on
          
          Defaults to loopback. Any non-loopback address requires `--allow-non-local`, because the
          listener exposes the upstream SOCKS5 server to everyone who can reach it.
          
          [default: 127.0.0.1]

      --upstream-host <UPSTREAM_HOST>
          Address of the upstream SOCKS5 server
          
          Separate from `--bind` so the proxy can listen on one interface while reaching the
          SOCKS5 server over another.
          
          [default: 127.0.0.1]

      --socks5-username <SOCKS5_USERNAME>
          Username for SOCKS5 username/password authentication
          
          Requires a password from `--socks5-password`, `--socks5-password-file`, or
          `KARAKO_SOCKS5_PASSWORD`. When this is omitted, only the no-authentication method is
          offered.

      --socks5-password <SOCKS5_PASSWORD>
          Password for SOCKS5 username/password authentication
          
          Prefer `--socks5-password-file` or `KARAKO_SOCKS5_PASSWORD`: command-line arguments are
          visible to other users through the process list.

      --socks5-password-file <PATH>
          File containing the SOCKS5 password
          
          One trailing newline (LF or CRLF) is stripped. Takes precedence over
          `KARAKO_SOCKS5_PASSWORD`, which takes precedence over `--socks5-password`.

      --proxy-username <PROXY_USERNAME>
          Username HTTP clients must send via Proxy-Authorization
          
          Turns the listener into an authenticated proxy. Requires `--proxy-password`. Clients
          then use `Proxy-Authorization: Basic <base64(user:pass)>`, which `curl` does
          automatically for `-x http://user:pass@host:port`.

      --proxy-password <PROXY_PASSWORD>
          Password HTTP clients must send via Proxy-Authorization
          
          Requests without a matching header receive `407 Proxy Authentication Required` with a
          Basic challenge.

      --allow-non-local
          Allow binding to a non-loopback address
          
          Without this flag the proxy refuses to start on a public interface. Anyone who can reach
          the listener can use the upstream SOCKS5 server, so pair this with client
          authentication.

      --no-via
          Omit the Via header from forwarded messages
          
          `Via` identifies this proxy in requests and responses (RFC 9110). Use this when the
          proxy should stay invisible to the origin server.

      --x-forwarded-for
          Add X-Forwarded-For with the client address
          
          A client-supplied `X-Forwarded-For` is discarded and replaced, so the header cannot be
          used to spoof the original address.

      --boostaler-cache
          Extend Cache-Control with stale-while-revalidate and stale-if-error
          
          Removes any existing `stale-while-revalidate`/`stale-if-error` directives, then appends
          `stale-while-revalidate=86400` (one day) and `stale-if-error=259200` (three days) to every
          response. A homage to the historic "Boostaler" browser extension, which aimed to keep
          pages usable while a site was slow or down.
          
          [alias: --stale-cache]

      --cors-allow-all
          Add permissive CORS headers to every response
          
          Replaces existing values for Access-Control-Allow-Methods, -Origin, -Headers,
          -Allow-Credentials, Expose-Headers, Timing-Allow-Origin and
          X-Permitted-Cross-Domain-Policies, on every method including non-OPTIONS. Any path ending
          in `CrossDomain.xml` or `ClientAccessPolicy.xml` (any letter case) receives a synthetic
          200 policy file instead of the upstream response. CONNECT tunnels carry opaque TLS after
          the proxy's 200, so headers inside the tunnel cannot be changed.
          
          [alias: --cors]

      --strip-security-headers
          Remove CSP and related security headers from every response
          
          Strips P3P, Alt-Svc, Strict-Transport-Security, X-Frame-Options, X-XSS-Protection,
          X-Content-Type-Options, Content-Security-Policy, Referrer-Policy and
          Public-Key-Pins-Report-Only, matching names in any letter case.
          
          [alias: --strip-csp]

      --tunnel-buffer-size <KIB>
          Copy buffer size per direction for CONNECT tunnels, in KiB
          
          Each direction of a tunnel gets its own buffer of this size. The default suits most
          links; raise it for high-bandwidth tunnels on high-latency paths. Accepted range: 5 to
          10240 KiB (5 KiB to 10 MiB).
          
          [default: 32]

      --socket-recv-buffer <KIB>
          Override SO_RCVBUF for all sockets, in KiB (default: 64-512 by role)
          
          Applies to the listener, SOCKS5 handshakes, upstream HTTP connections, and tunnels.
          Without this flag each role keeps its own default: 64 KiB for handshakes, 256 KiB for
          inbound and HTTP request sockets, and 512 KiB for CONNECT tunnels. Accepted range: 5 to
          10240 KiB (5 KiB to 10 MiB).

      --socket-send-buffer <KIB>
          Override SO_SNDBUF for all sockets, in KiB (default: 64-512 by role)
          
          Same coverage and defaults as `--socket-recv-buffer`: 64 KiB for handshakes, 256 KiB for
          inbound and HTTP request sockets, and 512 KiB for CONNECT tunnels. Accepted range: 5 to
          10240 KiB (5 KiB to 10 MiB). The kernel may clamp the requested value; the effective
          size is logged with `-v`.

  -v, --verbose...
          Increase log verbosity
          
          No flag logs one line per request. `-v` adds handshake, socket and timing details. `-vv`
          also dumps SOCKS5 packets as hex.

  -q, --quiet
          Only log errors
          
          Mutually exclusive with `--verbose`. Useful when the proxy runs in the background and
          only failures are interesting.

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

`-h` prints the summary above. `--help` prints an extra paragraph for every
option: what the two port flags mean, where the SOCKS5 password can come
from and in which order, why non-loopback binding needs an opt-in, how
the buffer sizes are interpreted, and what each response-rewriting flag
changes.

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

```txt
[SOCKS5] CONNECT example.com:443 (atyp=0x03, 18 bytes)
[SOCKS5] CONNECT out (18 bytes):
05 01 00 03 0B 65 78 61 6D 70 6C 65 2E 63 6F 6D
01 BB
[UPSTREAM] -> 200 OK in 647ms (12 response header(s))
[HTTP/1.1] GET / -> 200 OK in 2.48s
[TUNNEL] Tunnel example.com:443 closed normally after 707ms: client→target: 1893 bytes, target→client: 6006 bytes
```

<hr/>

## How to use / manual test recipes

The examples below assume a SOCKS5h server on
`127.0.0.1:9150` and start the proxy on port `18765`:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150
```

### Plain HTTP through the proxy

```txt
curl -x http://127.0.0.1:18765 http://example.com/
```

### HTTPS through a CONNECT tunnel

```txt
curl -k -x http://127.0.0.1:18765 https://example.com/
```

`-k` is only needed here because the curl build may not trust the test
certificate chain; HTTPS itself is tunnelled end to end.

### HEAD, 204 and 304 responses

```txt
curl -I -x http://127.0.0.1:18765 http://example.com/
curl -s -o NUL -w "%{http_code}\n" -x http://127.0.0.1:18765 http://httpbin.org/status/204
curl -s -o NUL -w "%{http_code}\n" -x http://127.0.0.1:18765 http://httpbin.org/status/304
```

### Streaming a large request body

```txt
# 1 MiB of random data
python -c "import os;open('body.bin','wb').write(os.urandom(1024*1024))"
curl -x http://127.0.0.1:18765 --data-binary @body.bin http://httpbin.org/post
```

The proxy forwards the body as it arrives; it is never collected in
memory.

### Verify upstream connection reuse

Send two requests to the same origin in one command. With `-v` you
should see a single `SOCKS5 Handshake complete` line for that origin:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 -v
curl -s -o NUL -x http://127.0.0.1:18765 http://example.com/ http://example.com/
```

### Inspect the `Via` header

```txt
curl -s -D - -o NUL -x http://127.0.0.1:18765 http://example.com/ | findstr /i "^via:"
```

or

```txt
curl --silent --output NUL --write-out "Via: %header{via}\n" --proxy http://127.0.0.1:18765 http://example.com/
```

### Listen on IPv6 loopback

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 --bind ::1
curl -x "http://[::1]:18765" http://example.com/
```

### Upstream SOCKS5 on another host

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 --upstream-host 192.168.1.10
```

### Username/password authentication (RFC 1929)

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --socks5-username alice --socks5-password secret
```

some proxies accepts any username/password and uses the username as an isolation
key; other SOCKS5 servers may validate them.

### Password from a file or environment

Command-line arguments are visible to other users through the process
list. Prefer one of these instead:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --socks5-username alice --socks5-password-file ./socks.pw

KARAKO_SOCKS5_PASSWORD=secret karako_http_socks5h_proxy -p 18765 -u 9150 \
  --socks5-username alice
```

A trailing newline in the password file is stripped.
`--socks5-password-file` takes precedence over the environment variable,
which takes precedence over `--socks5-password`.

### Require clients to authenticate

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --proxy-username alice --proxy-password secret

curl -x http://alice:secret@127.0.0.1:18765 http://example.com/
```

Requests without the matching `Proxy-Authorization: Basic` value receive
`407 Proxy Authentication Required`.

### Listen beyond loopback

Binding to anything other than a loopback address requires an explicit
opt-in, because the listener exposes the upstream SOCKS5 server to the
network:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 --bind 0.0.0.0 --allow-non-local
```

Combine it with `--proxy-username`/`--proxy-password` unless the network
is trusted.

### Forwarding headers

`Via` is added by default. Pass `--no-via` to keep the proxy out of the
forwarded messages. Pass `--x-forwarded-for` to add
`X-Forwarded-For: <client-ip>`; a client-supplied value is replaced so it
cannot be used to spoof the original address.

### Response header rewriting

Three opt-in flags rewrite the response head before it leaves the proxy.
They apply to every response the proxy emits, in either HTTP version:
forwarded responses, `407` challenges, error responses such as `400` and
`502`, the `200` that answers `CONNECT`, and the synthetic cross-domain
policy files. The flags are independent and can be combined.

None of them can reach inside a `CONNECT` tunnel. After the proxy replies
`200` to `CONNECT`, the connection carries the client's TLS session
end to end; the proxy only copies bytes and never sees the inner
response headers. For HTTPS traffic only the outer `200` head is
rewritten. Use plain HTTP, or terminate TLS in front of the proxy, if the
inner headers matter.

#### `--boostaler-cache` (alias `--stale-cache`)

Every response gets `Cache-Control: stale-while-revalidate=86400,
stale-if-error=259200`. Existing `stale-while-revalidate` and
`stale-if-error` directives are removed first, so a weaker upstream value
cannot win; every other directive is kept. For example,
`public, max-age=60, stale-while-revalidate=10, immutable` becomes
`public, max-age=60, immutable, stale-while-revalidate=86400,
stale-if-error=259200`.

The two directives tell a shared or browser cache how long it may serve
an expired response:

- `stale-while-revalidate=86400` — for one day the cache may answer from
  its copy immediately, then refresh in the background.
- `stale-if-error=259200` — for three days the cache may keep answering
  from its copy if the origin returns an error or is unreachable.

The name is a homage to the historic "Boostaler" browser extension,
which tried to keep pages usable while a site was slow or down. See
[RFC 5861](https://www.rfc-editor.org/rfc/rfc5861) for the directives.

#### `--cors-allow-all` (alias `--cors`)

Every response, on every method (not only `OPTIONS`), gets these headers,
replacing any value the origin sent:

| Header | Value |
|---|---|
| `Access-Control-Allow-Methods` | `CONNECT,DELETE,GET,HEAD,OPTIONS,PATCH,PING,POST,PUT,TRACE` |
| `Access-Control-Allow-Origin` | `*` |
| `Access-Control-Allow-Headers` | `Accept-Ranges,Authorization,Content-Disposition,Content-Encoding,Content-Length,Content-Range,Content-Type,ETag,Last-Modified,Range,X-Requested-With` |
| `Access-Control-Expose-Headers` | same list as `Access-Control-Allow-Headers` |
| `Access-Control-Allow-Credentials` | `true` |
| `Timing-Allow-Origin` | `*` |
| `X-Permitted-Cross-Domain-Policies` | `all` |

Browsers ignore `Access-Control-Allow-Credentials: true` together with
`Access-Control-Allow-Origin: *`; the pair is sent verbatim because that
is the historical permissive profile. Cookies and HTTP authentication
still need an explicit origin, so relax the value yourself if you rely on
credentialed requests.

The same flag also mocks the two legacy cross-site policy files. A request
whose path's last segment is `CrossDomain.xml` or `ClientAccessPolicy.xml`
(in any letter case, for example `/foo/crossdomain.XML`) is answered
locally with `200 OK`, `Content-Type: text/x-cross-domain-policy`, a
recalculated `Content-Length`, and the corresponding body instead of the
upstream file:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 --cors-allow-all
curl -s -D - -x http://127.0.0.1:18765 http://example.com/CrossDomain.xml
```

Both bodies are compiled into the binary. The Flash policy permits
`domain="*"` and `to-ports="*"`; the Silverlight policy grants `domain
uri="*"` access to `/` including subpaths. Paths that merely contain the
name, such as `/notCrossDomain.xml`, are forwarded upstream unchanged.

#### `--strip-security-headers` (alias `--strip-csp`)

Removes these response headers, matching names in any letter case:

```txt
p3p
alt-svc
strict-transport-security
x-frame-options
x-xss-protection
x-content-type-options
content-security-policy
referrer-policy
public-key-pins-report-only
```

Use it when an origin pins HSTS, blocks framing or scripts, or advertises
`Alt-Svc` that the SOCKS5 path cannot reach. It is intentionally a fixed
list; other `Content-Security-Policy-Report-Only` or `Public-Key-Pins`
variants are left alone.

#### Manual check

Any origin that sends cache, CORS or security headers will show the
difference. A one-file local fixture works too: serve it directly on
`127.0.0.1:18888`, then start the proxy with all three flags.

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --boostaler-cache --cors-allow-all --strip-security-headers

curl -s --noproxy "" -D - -o NUL -x http://127.0.0.1:18765 \
  http://127.0.0.1:18888/
```

`curl` honors `NO_PROXY` for localhost, so `--noproxy ""` (or an empty
`NO_PROXY`) is needed to send the request through the proxy. The output
should show the two stale directives appended, the seven CORS headers,
and none of the stripped security headers. Request a policy path to see
the mock instead of the upstream file:

```txt
curl -s -x http://127.0.0.1:18765 http://127.0.0.1:18888/CrossDomain.xml
```

### Buffer tuning

CONNECT tunnels copy with a 32 KiB buffer per direction by default:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 --tunnel-buffer-size 64
```

Socket buffers keep per-role defaults: 64 KiB for the SOCKS5 handshake,
256 KiB for inbound and HTTP request sockets, and 512 KiB for CONNECT
tunnels. Override all of them for a high-throughput tunnel:

```txt
karako_http_socks5h_proxy -p 18765 -u 9150 \
  --tunnel-buffer-size 64 --socket-recv-buffer 512 --socket-send-buffer 512
```

Sizes are in KiB. Every buffer option accepts **5 to 10240 KiB**
(5 KiB to 10 MiB); values outside that range are rejected at startup.
The defaults are:

| Option | Default |
|---|---|
| `--tunnel-buffer-size` | 32 KiB per copy direction |
| `--socket-recv-buffer` / `--socket-send-buffer` | per role: 64 KiB handshake, 256 KiB inbound/HTTP, 512 KiB tunnel |

The effective values are logged with `-v`.

### Query strings in logs

The default one-line-per-request log prints the path only, so tokens or
session IDs in query strings do not end up in the console. Use `-vv` when
you need the full upstream URI while debugging.

### Logging levels

```txt
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
- Forwarding correctness: `Content-Length` is preserved in both directions, `Host` omits the default port, and absolute-form `https://` targets are rejected so HTTPS always uses CONNECT
- SOCKS5h compliant: Uses domain names instead of IP addresses (privacy-friendly)
- SOCKS5 authentication: optional RFC 1929 username/password support, with a password file or environment variable
- Client authentication: optional inbound `Proxy-Authorization: Basic` for the HTTP listener
- Privacy: `--no-via` omits the proxy identity, query strings are not logged by default
- Response rewriting (opt-in): `--boostaler-cache` extends stale serving, `--cors-allow-all` opens CORS and mocks the legacy cross-domain policy files, `--strip-security-headers` removes CSP/HSTS/framing headers
- Tunable buffers: `--tunnel-buffer-size`, `--socket-recv-buffer`, `--socket-send-buffer`
- Error mapping: `400` for bad client input, `502` for upstream failures, `504` for upstream timeouts
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
  | GET /path HTTP/1.1           |                              |
  | Host: example.com            |                              |
  |─────────────────────────────>|                              |
  |                              | 1. Extract host:port         |
  |                              |    (example.com:80)          |
  |                              |                              |
  |                              | 2. Connect to SOCKS5         |
  |                              | ────────────────────────────>|
  |                              |<────────────SOCKS5 OK────────|
  |                              |                              |
  |                              | 3. Forward HTTP request      |
  |                              |    to example.com:80         |
  |                              |─────────────────────────────>|
  |                              |                              |
  |                              | 4. Receive HTTP response     |
  |                              |<─────────────────────────────|
  |          HTTP Response       |                              |
  |<─────────────────────────────|                              |
  |                              |                              |

```

<hr/>

```txt
================================================================================
              HTTP/1.1 CONNECT TUNNEL FLOW (HTTPS)
================================================================================

Client                         Proxy                    Upstream Target
  |                              |                              |
  | CONNECT example.com:443      |                              |
  |─────────────────────────────>|                              |
  |                              | 1. Extract host:port         |
  |                              |    (example.com:443)         |
  |                              |                              |
  |                              | 2. Connect to SOCKS5         |
  |                              |─────────────────────────────>|
  |                              |<────────────SOCKS5 OK────────|
  |                              |                              |
  |        200 Connection        |                              |
  |<─────────────────────────────|                              |
  |                              | 3. Upgrade & tunnel          |
  |◄─── HTTPS tunnel active ───────────────────────────────────►|
  |                              |                              |
  | CLIENT_HELLO (TLS)           |                              |
  |─────────────────────────────>|─────────────────────────────>|
  |                              |                              |
  |          SERVER_HELLO        |                              |
  |<─────────────────────────────|<─────────────────────────────|
  |                              |                              |
  |  [bidirectional HTTPS copy]  |                              |
  |◄───────────────────────────────────────────────────────────►|
  |                              | (5-minute idle timeout)      |
  |                              |                              |
  |        [connection close]    |                              |
  |─────────────────────────────>|─────────────────────────────>|
  |                              |                              |

```

<hr/>

```txt
================================================================================
           SOCKS5h HANDSHAKE SEQUENCE (Domain Resolution)
================================================================================

HTTP Proxy (Client)            SOCKS5h Server (Upstream)
       |                                  |
       | GREETING: [VER=5, NMETHODS=1,    |
       | METHOD=0 (no auth)]              |
       |─────────────────────────────────>|
       |                                  |
       |          METHOD SELECTION:       |
       |          [VER=5, METHOD=0]       |
       |<─────────────────────────────────|
       |                                  |
       | CONNECT REQUEST:                 |
       | [VER=5, CMD=1, RSV=0,            |
       |  ATYP=3 (domain),                |
       |  DLEN, DNAME..., PORT]           |
       |─────────────────────────────────>|
       |                                  |
       |   (DNS lookup on server)         |
       |   (Connect to target)            |
       |                                  |
       |       CONNECT RESPONSE:          |
       |       [VER=5, REP=0, RSV=0,      |
       |        ATYP, BIND_ADDR, PORT]    |
       |<─────────────────────────────────|
       |                                  |
       | ✓ Tunnel established             |
       | Ready for HTTP request           |
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

SOCKS5 handshake         ──>  SOCKS_HANDSHAKE
                              • 10s timeout
                              • Handshake-specific opts

HTTP request forward     ──>  SOCKS_HTTP_REQUEST
                              • Request-specific opts

CONNECT tunnel           ──>  SOCKS_TUNNEL
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
       ├─> Upstream Timeout (headers or handshake)
       |   └─> 504 Gateway Timeout
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

