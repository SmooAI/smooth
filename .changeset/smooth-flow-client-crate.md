---
'@smooai/smooth': patch
---

New `smooai-smooth-flow-client` crate (th-3e6020): the SmoothFlow client rules written once, with conformance vectors. It covers the pane tree, close decisions, tab titles, center-tab gating, the Directory field helpers, and fleet grouping. The Linux/Windows desktop app uses it directly, and the Mac, iOS and Android apps replay its JSON vectors (`spec/vectors/*.json`) so none of them can drift from the Client Spec. Writing it down fixed a focus bug: moving focus down from the top of a stacked split jumped sideways to a tall neighbouring pane, because a pane only had to have its centre in the right direction. Now only a pane beyond the focused pane's edge counts, and an exact tie goes to the lower pane id.
