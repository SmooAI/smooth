---
'@smooai/smooth': patch
---

Release builds of `smooth-daemon` for the Big Smooth and SmoothFlow installers now link OpenSSL statically (`OPENSSL_STATIC=1`). The bundled daemon used to dlopen `/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib` (web-push → ece/curl → openssl-sys), so it only started on Macs with Homebrew openssl and was rejected under the hardened runtime. Both publish workflows now fail if the binary still links a Homebrew dylib. th-b4e4de.
