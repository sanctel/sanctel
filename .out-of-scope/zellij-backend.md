# Zellij as PTY backend

**Decision:** Rejected after a focused spike. Sanctel stays on tmux.

**Reason:** The original case for migrating (cleaner architecture, lower
quirks profile, better embedding fit) did not survive empirical
integration. Both backends have structural bugs at comparable density,
performance is parity, and zellij introduces a supervised-daemon process
model that tmux does not require. The only argument that would tip the
verdict — native Windows support — does not apply because Windows is not
on sanctel's roadmap (see `.out-of-scope/windows-support.md`).

**Prior requests:** Issue #16 (spike), with findings posted as a comment
on that issue.

---

## Background

Sanctel originally chose tmux as its PTY backend (ADR-0002). Issues #11–#15
surfaced five integration bugs in a single week, four of them rooted in
tmux's session/client model. A side-by-side analysis suggested zellij
might be a better structural fit: stable per-pane IDs, an explicit
WebSocket embedding protocol (`zellij web`), disk-based session
serialization, and native Windows support via ConPTY.

Issue #16 commissioned a 2–3 day spike to validate or refute the
hypothesis through real integration work. The spike was conducted on
`main` behind a `SANCTEL_BACKEND=zellij` env-flag, with the tmux code
left intact alongside parallel zellij modules.

## Spike results

7/7 functional acceptance criteria pass; 1/1 performance criterion
passes. Full results in the spike findings comment on issue #16.

The valuable finding is not the criterion pass rate — it's the three
structural surprises that emerged during integration:

1. **`zellij list-sessions -s` hangs indefinitely on stale session sockets.**
   The CLI tries to handshake with each socket in the dir; dead-process
   sockets never respond. Upstream tracked as zellij#2074, open 2.5
   years with no fix. Sanctel had to write its own socket-dir scanner
   with connect-probe cleanup to work around this.

2. **Pre-created sessions cause `zellij` to close the WebSocket
   prematurely.** Calling `zellij attach --create-background <name>`
   followed by a WebSocket attach to that name produces a `Normal`
   close immediately after the handshake. The path zellij's own browser
   client uses (WS-handler-creates-the-session-lazily) does not have
   this problem. Discovered by manual debugging; not documented
   anywhere in zellij's protocol notes.

3. **Webview hydrate races content-rect layout, producing degenerate
   grid sizes at `terminal_attach`.** Not zellij-specific in cause, but
   only surfaced as a visible bug on zellij because zellij's screen
   re-emission semantics differ from tmux's. Fixed in
   `waitForRealContainerSize` in the frontend; the fix stays in even
   though zellij is gone, because the same race could in principle bite
   tmux too.

None of these were predictable from source-code analysis. The spike
budget covered them, but the broader lesson — "the framing 'zellij is
cleaner' is unsafe to act on without empirical evidence" — is the
durable artifact.

## The verdict shape

| Question | Answer |
|---|---|
| Functional parity with tmux? | Yes (7/7) |
| Performance parity? | Yes (1.499s vs 1.598s on the printf benchmark, ~6% faster than tmux) |
| Structural improvement? | No — different bugs, similar density |
| Operational simplicity? | Worse — adds daemon process, port allocation, auth flow |
| Native Windows support? | Yes (the one decisive advantage) |

**With Windows on roadmap:** zellij wins (Windows is the only path).
**Without Windows on roadmap:** tmux wins by a moderate margin across
multiple small axes (memory ~5× lower per session; one server-process
vs N; sessions survive multiplexer version upgrades; better embedding
CLI primitives via `pipe-pane`; no upstream list-sessions hang).

Windows is not on sanctel's roadmap. Tmux wins.

## What we actually built that we shouldn't have

A note for future maintainers reconsidering this decision: the spike's
zellij integration went through `zellij web`'s WebSocket protocol. This
was the wrong architectural choice. `zellij web` is designed for
**browser embedding** — TCP port, session_token cookies for cross-process
trust, WebSocket because that's what browsers can speak. Sanctel is a
desktop app sharing the same machine and user as zellij; none of those
costs are necessary.

The simpler integration would have been to spawn `zellij attach -s name`
directly in a PTY (via `portable-pty`, which sanctel already uses for
tmux). Roughly 200 LOC of glue, identical shape to the tmux backend,
zero daemon supervision, zero auth flow, zero WebSocket protocol. The
tradeoff would be that zellij's UI overlay (tab bar, status bar) appears
in the xterm — but that already happens in the WebSocket integration
too, so we got the heavy path AND the visible UI: worst of both.

If sanctel ever reconsiders zellij, **the first thing to evaluate is
direct PTY attach, not `zellij web`**. The WebSocket route exists for
browser embedding; sanctel is not a browser.

## When to reconsider this decision

Re-open if any of these become true:

- **Windows enters the roadmap.** This is the load-bearing condition.
  Before re-spiking zellij in that case, evaluate **psmux** first — a
  Windows-native ConPTY-based Rust implementation that claims tmux CLI
  compatibility including `pipe-pane` and `send-keys`
  (https://github.com/psmux/psmux). If psmux's compatibility holds, the
  migration is single-binary-swap per platform with the same
  `attach_tab_to_tmux` integration. That's a structurally smaller change
  than a re-do of the zellij spike.

- **Tmux's control mode protocol breaks across a tmux version upgrade.**
  Currently stable for many years; if that stability ever breaks,
  zellij's stated public API becomes more attractive.

- **Sanctel needs cross-network terminal access (multi-device, mobile,
  cloud-hosted).** In that case the WebSocket layer should live in
  **sanctel itself** (axum / warp on an authenticated endpoint), not in
  the backend. tmux + a sanctel-owned WebSocket wrapper (similar shape
  to `ttyd`) is the path; zellij's `zellij web` is not the right primitive
  because its trust model is local-only.

- **Sanctel's persistence requirements grow beyond what tmux can offer.**
  Currently tmux + the SQLite TabRecord + claude's `--resume` covers the
  case. If sanctel ever needs structured event streams, replayable
  recordings, container-isolated sessions, or first-class
  detach-from-one-machine-attach-from-another, evaluate cloud-IDE-shaped
  backends (firecracker microVMs + ttyd, AWS Bedrock AgentCore browsers,
  etc.) — zellij doesn't solve any of these meaningfully better than tmux.

## Follow-ups from the spike (kept open)

Bugs #30 and #31 (chat tab `agent_session_id` capture and post-create
update) are pre-existing sanctel bugs that the spike surfaced. They
affect tmux too and remain open.

Bugs #32 and #33 (zellij-specific orphan-process reaper and WebSocket
reconnect on daemon respawn) are closed alongside the rejection — they
only existed if we kept zellij.

## Spike code preservation

The final spike state is tagged `spike/zellij-end` for reference. The
deletion happens in a subsequent commit; `git checkout spike/zellij-end`
restores the integration if anyone needs to read it later.
