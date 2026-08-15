---
'@smooai/smooth': patch
---

Big Smooth: new `get_weather` tool — current conditions + a 3-day forecast for any place (or the daemon's own location when none is given). Keyless (Open-Meteo geocoding + forecast, keyless IP fallback), HTTP via the house `smooai-fetch` client. Named `get_*` so Auto mode treats it as a read and never prompts. imperial by default, metric on request.
