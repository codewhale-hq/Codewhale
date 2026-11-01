Borrowed @deepseek-ai/dsh-agent-presets 0.1.6-alpha.1, MIT.
Source: deepseek-ai/deepseek-harness 0d1f50007f9bca3f52b06e1c3074fa14d5fb0720.

This legacy preset adapter retains that exact source pin during the rc.5
upgrade. Upstream later split preset discovery and mounting into other
packages; those runtime packages are not imported by this upgrade. The
loader/include/group composition underneath it now uses the rc.5 pin in
`../UPSTREAM.json`.
Codewhale delta: discovery and metadata require receipt-backed IO; no ambient
filesystem, home root, Node package walk, Agent, Session, settings store or
standing-mount graph is introduced. Include/Loader algorithms are the existing
pinned upstream implementations. Group/layer/conditional semantics stay there.

Discovery also fixes the pinned `Infinity - Infinity` comparator defect so
two presets without declared order use the documented stable id ordering.
The focused root-precedence/broken-row test covers this source correction.
