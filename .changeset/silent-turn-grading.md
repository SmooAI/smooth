---
'@smooai/smooth': patch
---

bench: a silent turn that did work is gradeable (build-shaped turns no longer INCONCLUSIVE)

The convo suite discarded any turn whose assistant text was empty as "nothing to
grade" — even when it had called a dozen tools. A build-shaped turn (scaffold,
install, write files) runs for minutes and produces no prose until the end, so a
complete and correct `create-next-app` scaffold was thrown away as INCONCLUSIVE and
had to be read off the filesystem by hand.

A silent turn is now only ungradeable when it also called no tools; tool calls are
evidence and the judge already receives the transcript. The same run now scores
PASS 5/5/5/5.
