---
'@smooai/smooth': patch
---

SmoothFlow no longer shares Big Smooth's relay identity (th-a1bb12). The app mints and keeps its own relay device id (`~/.smooth/smoothflow-relay-device-id`) and starts its child daemon with `SMOOTH_RELAY_DEVICE_ID`, `SMOOTH_RELAY_LABEL="<host> · SmoothFlow"` and the new `SMOOTH_RELAY_KIND=flow`, so a phone's device list shows Big Smooth and SmoothFlow as two peers instead of one identity whose presence flapped between two sockets. `smooth-daemon` also holds an advisory lock per relay device id: a second daemon on the same machine that resolves the same id logs an error and stays off the relay until the first exits.
