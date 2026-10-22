---
'@smooai/smooth': patch
---

SmoothFlow tells an interrupted session from a crashed one (SMOODEV-4113). After a clean daemon shutdown (quitting the app) or a reboot, agents whose conversation can be resumed come back with their harness's resume, 300 ms apart, without spending the crash-retry budget, on tmux and the pty host alike. An agent with no harness session id is never relaunched with its prompt, after an interruption or a crash: it is left `dead` with attention `restorable`. A `/clear` in a SmoothFlow-launched agent rebinds the row to the new harness session id.
