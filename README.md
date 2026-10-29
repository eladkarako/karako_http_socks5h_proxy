# `karako_http_socks5h_proxy`

gives your programs a way to use an upstream `SOCKS5h` server,  
including its built-in DNS resolution on remote. meaning your own machine's DNS/HOSTS will not be used.

you can use it with aria2c, yt-dlp, ffmpeg, wget, curl (although it has `SOCKS5h` support),  
native batch-files scripts with `HTTPS_PROXY` and `HTTP_PROXY`. 

most programs will use HTTP1.1 `CONNECT`, and once the TCP tunnel established it supports any traffic  
that the `SOCKS5h` upstream server would support (HTTP, HTTPS, ...).  
you can set firefox and edge browsers to use it as HTTP proxy, and it works very well.

by design limitations: hostname is hard-coded by design to be `127.0.0.1`. both `SOCKS5h` upstream server and this program must run on same machine.  

known issues/bugs: in `HEAD` (and probably `HTTP 204` and `HTTP 304`) there is zero length body which trigger some quirk in `hyper`. I'm trying to work around it.  

binaries provided, supports many OS and architectures.

still under development so a lot of debug information is written..

<a href="https://paypal.me/31adkarak0" target="_blank" rel="noopener noreferrer">
  <img src="resources/logos/paypal1.svg" alt="Donate via PayPal">
  <br />
  <img src="resources/logos/paypal2.png" alt="PayPal Donation">
</a>

<br/>
<hr/>
<br/>

