---
'@smooai/smooth': minor
---

`smoo calling numbers available|attach|buy` gets a phone number onto the org. `available` lists the numbers the org's Twilio account already owns (attach, no charge) and numbers for sale with Twilio's monthly price. `attach` never buys. `buy` shows the route's price for that exact number (its own country, local / toll-free / mobile), refuses without `--accept-price` at or above it, and asks for confirmation (`--yes` in scripts). Buying needs calling admin and billing management.
