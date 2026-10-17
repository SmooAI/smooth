---
'@smooai/smooth': minor
---

`smoo credits` manages prepaid AI credits for the active org: bare `smoo credits` (alias `balance`) shows the balance, this month's credit use and auto-reload; `smoo credits buy 5|10` (alias `top-up`) opens a Stripe Checkout for a pack; `smoo credits auto-reload status|on|off` reads or changes auto-reload, and `on` previews until `--confirm`. When a turn runs out of AI budget on the Smoo AI gateway, `th code`, `th harness add --agentic` and the web dashboard now say "Out of AI budget" with the top-up link instead of showing the gateway's raw `Budget has been exceeded` JSON. Big Smooth's Family plan onboarding (`th model setup`, `th doctor --onboard`, the web first-run card) mentions that trial AI usage can be topped up.
