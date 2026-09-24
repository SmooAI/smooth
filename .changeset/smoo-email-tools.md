---
'@smooai/smooth': minor
---

New `smoo email` commands and matching `th mcp serve` tools for email deliverability and signatures (SMOODEV-3272): `check` grades any domain's SPF, DKIM, DMARC, MX, blocklist, MTA-STS and BIMI records with what to change; `dmarc`, `sources` and `tls` read the DMARC and TLS reports for your org's domains; `signatures` shows which domains sign and who is signed. They call the same API as the web app, as you.
