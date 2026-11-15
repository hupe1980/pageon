# Threat model

This document defines what pageon is meant to defend against, what it trusts and where it
stops. It lists every known threat class with its status and evidence, including the open
ones. Report a suspected vulnerability privately as described in [SECURITY.md](https://github.com/hupe1980/pageon/blob/main/SECURITY.md).

## Release status

This describes pageon 0.1.0, a pre-release. The model was built with
[ThreatSpec](https://github.com/hupe1980/spec-kit-threatspec) (STRIDE, the OWASP Top 10 for LLM
and for Agentic Applications 2026, MITRE ATLAS, MAESTRO) over every feature. Every status below
is checked against the code and its tests; classes marked *reproduced* were also confirmed by
running pageon.

pageon runs each page in a sandboxed content process built with
[procage](https://github.com/hupe1980/procage) (Seatbelt on macOS; Landlock, seccomp and
namespaces on Linux). A broker process keeps the network, the egress policy, the secrets vault,
the guard client and the recorder, and performs every request on the page's behalf. All page
text in the agent's views is quoted.

It does **not**, in this release:

- authenticate clients of the CDP, BiDi and MCP HTTP endpoints beyond loopback, `Host` and
  `Origin` checks and the per-run secret in the printed URL (P-11);
- enforce origin sets, approvals or a guard unless the operator configures them; without them,
  every request the page or a hijacked agent makes is sent (P-05);
- recognise secrets that a page transforms (reverses, splits, re-encodes) before sending or
  echoing them (P-06, P-07);
- encrypt recordings and saved state files: they hold cookies and response bodies, and are
  protected only by being readable by the operator alone (P-08).

## Adversary

- **A hostile page**: any site the agent visits, its scripts, third-party scripts and
  cross-origin frames inside it, and the servers that answer. It is adaptive and writes text
  meant for the model (indirect prompt injection), for pixels (vision models) and for the guard.
- **A hijacked agent**: the model driving pageon, after following injected page text. It can
  call every action and tool the operator exposed, with any arguments.
- **A local process** on the same host, of the same or another user.
- **A network attacker** on the path between pageon and a site.
- **A compromised content process**: page code that exploited the engine (V8, the bindings,
  layout) and now runs arbitrary code inside the sandbox.

## Assets

- Secrets in the vault and TOTP seeds; the Web Bot Auth signing key.
- Session state: cookies, localStorage and sessionStorage, including imported logins.
- The operator's files and programs, the host's private network and cloud metadata endpoints.
- The ability to send requests on the operator's behalf, above all writes.
- What the model reads: views, outcomes, screenshots, tool results.
- Recordings and saved state files.
- Control of the browser session through MCP, CDP, BiDi and the takeover view.

## Trusted

- The operator and the configuration they give: origin sets, secrets, the approver, the guard
  program, the identity file.
- The agent host process that starts pageon (an MCP host, the Python package, a CDP client), or a
  program that embeds it through `pageon::Config`, which assembles exactly the session the CLI does.
- The kernel, the OS sandbox facilities, and procage.
- The person who uses the takeover live view.

## In scope

- Everything a hostile page can do through the engine, the network and the agent's views.
- Everything a hijacked agent can do through pageon's actions and tools.
- Leaks of secrets, session state and files to pages, to the model, to protocol clients and to
  disk.
- Local processes reaching pageon's loopback endpoints.
- A compromised content process attacking the broker.

## Out of scope

- Bugs in the kernel, the OS sandbox or procage itself (see procage's own threat model).
- Side channels, and denial of service beyond the limits listed under P-20.
- A malicious operator, agent host or guard program.
- What a site does with data the operator allowed pageon to send it.
- Whether the model obeys quoted page text. pageon quotes and marks it; deciding not to follow
  it is the harness's and the model's job (P-09).

## Strength

The sandbox reduces what engine exploits can reach, and the broker's checks hold however the
model behaves. They do not make a hijacked agent harmless: with no origin sets, approver or
guard, an agent that follows a page's instructions can send anything to anyone. Origin sets are
the hard boundary; the secrets check at egress is defence in depth against common encodings,
not against a page that transforms a value on purpose.

## Threat classes

Ids are stable and never reused. The fields are:

- **Disposition**: `mitigate` (pageon is designed to prevent it), `harness` (the operator or
  agent harness must handle it, with help from pageon), `accept` (a recorded risk).
- **Status**: `mitigated` only with test evidence and no known open variant; `partial` when
  variants are open (listed under Open variants); `open` when no control covers it;
  `documented` when the harness owns it.
- **Evidence**: tests in the repository (`file::test`) that attempt the attack and assert the
  refusal.

| Id | Class | Description | Sev | Disposition | Status | Evidence |
|---|---|---|---|---|---|---|
| P-01 | Local files through `file:` | A web page loads `file:` URLs through iframes, scripts, stylesheets or `fetch`, and a hijacked agent opens them with `browser_open`. The broker reads them; the policy exempts `file:`, even with origin sets. File contents appear in the agent's view, scripts run, and load/error events reveal which files exist. *Reproduced, then fixed:* only `file:` documents load `file:` URLs, and agents need `--allow-files`. | critical | mitigate | mitigated | crates/pageon/tests/hardening.rs::web_pages_cannot_read_local_files, ::a_redirect_cannot_lead_onto_the_disk, ::local_pages_keep_their_own_files, ::agents_need_allow_files_for_local_pages, ::cdp_navigation_to_local_files_is_refused |
| P-02 | Private network (SSRF) | Requests to loopback, private ranges, link-local and metadata endpoints, in any address encoding, through redirects and through DNS answers. | critical | mitigate | partial | crates/pageon/tests/network.rs::us3_private_destinations_are_blocked_by_default, ::us3_blocked_loads_say_what_to_do; crates/pageon-env/tests/live.rs::private_addresses_are_refused_before_connecting; crates/pageon-env/src/addr.rs::tests::transition_and_documentation_addresses; crates/pageon/tests/hardening3.rs::transition_addresses_are_private |
| P-03 | Engine exploit escapes the cage | Code that compromised the content process reads files, writes, connects or executes programs. | high | mitigate | mitigated | crates/pageon/tests/sandbox.rs::the_cage_holds |
| P-04 | Compromised content process lies to the broker | The broker trusts fields the content process sends: `allow_private`, labels and cause, the origin a secret is typed into, recorded steps. The cookie jar also lives in the content process. The broker decides `allow_private` from the command it serves, releases a secret once for the handle its own type action types, records steps it describes itself, drops `secret:` labels, and keeps `HttpOnly` cookies out of the content process. | high | mitigate | mitigated | crates/pageon/src/sandbox.rs::tests::allow_private_comes_from_the_served_command, ::secrets_only_for_the_typed_handle_once, ::steps_come_from_the_served_command, ::http_only_cookies_stay_in_the_broker; crates/pageon/tests/broker.rs::http_only_sessions_work_caged_with_the_cookie_in_the_broker, ::a_caged_session_with_http_only_cookies_replays |
| P-05 | Unapproved egress | Requests outside the origin sets, writes without approval, and first contacts the operator never saw. | critical | mitigate | partial | crates/pageon/tests/policy.rs::policy_matrix_in_process_and_caged; crates/pageon/tests/guard.rs::the_guard_sees_metadata_and_decides; crates/pageon/tests/hardening.rs::an_origin_without_a_scheme_is_a_usage_error |
| P-06 | Secret exfiltration at egress | A page sends a vault secret to an origin it is not bound to. | high | mitigate | partial | crates/pageon/tests/guard.rs::a_secret_never_leaves_for_another_origin; crates/pageon/src/secrets.rs::secrets_are_found_in_every_encoding |
| P-07 | Secrets reach the model or a protocol client | A secret appears in views, outcomes, screenshots, tool results, toolset reads (console, network), or CDP and BiDi messages. Secrets are filled at once as a password manager fills a field, so no prefix of one ever exists in the page to be logged. | high | mitigate | partial | crates/pageon/tests/secrets.rs::secrets_reach_the_server_and_nothing_else; crates/pageon/tests/toolsets.rs::secrets_stay_out_of_every_result; crates/pageon/tests/paint.rs::secrets_never_reach_pixels; crates/pageon/tests/bidi.rs::secrets_never_reach_bidi_messages; crates/pageon/tests/hardening.rs::secrets_are_redacted_session_wide |
| P-08 | Secrets and session data at rest | Recordings and saved state files hold cookies, response bodies and encoded or plain-typed credentials, readable by other local users. | high | mitigate | partial | crates/pageon/tests/identity.rs::recordings_never_hold_signatures_or_keys; crates/pageon/tests/hardening.rs::recordings_and_state_files_are_private; crates/pageon-env/src/record.rs::cookie_values_are_not_recorded, ::a_body_limit_stops_the_recording |
| P-09 | Indirect prompt injection | Page text steers the model. pageon quotes all page text, marks hidden text, input traps and third-party frames, and refuses to act on hidden fields; a quoted instruction can still persuade a model. | high | mitigate, harness | partial | crates/pageon-agent/src/quote.rs::engine_vocabulary_stays_inside_quotes; crates/pageon/tests/mcp.rs::browsing_through_mcp; crates/pageon/tests/hardening.rs::page_error_text_cannot_forge_view_lines, ::clipped_text_is_marked_hidden_and_left_out_of_markdown, ::download_names_cannot_forge_outcome_lines |
| P-10 | Injection through pixels | Text a person cannot see (white, tiny, in third-party frames) instructs a vision model. `--mask` paints over it. | high | mitigate | partial | crates/pageon/tests/paint.rs::masks_cover_what_people_cannot_see_and_nothing_else |
| P-11 | Endpoint hijack | A web page (cross-site WebSocket or POST, DNS rebinding) or a local process drives the CDP, BiDi or MCP HTTP endpoint. | high | mitigate | partial | crates/pageon/tests/cdp.rs::the_endpoint_refuses_foreign_hosts_and_origins, ::idle_connections_and_quoted_commands_cannot_stop_the_endpoint; crates/pageon/tests/bidi.rs::the_endpoint_is_caged_and_refuses_like_cdp; crates/pageon/tests/cdp.rs::the_endpoint_answers_only_its_printed_url, ::allowing_every_origin_warns; crates/pageon/tests/mcp.rs::streamable_http_serves_2026_and_refuses_the_rest |
| P-12 | Takeover view | Another party claims the live view, or what the person types leaks. What the person typed (passwords, codes) reached the agent as `press` outcomes and was recorded. *Reproduced, then fixed:* takeover input is the person's, not a step. | critical | mitigate | mitigated | crates/pageon/tests/takeover.rs::a_person_logs_in_through_the_live_view, ::nobody_comes_and_it_times_out; bindings/python/tests/test_browser.py::test_takeover_hands_the_page_to_a_person; crates/pageon/tests/mcp.rs::the_live_view_url_goes_to_the_person_never_to_the_model; crates/pageon/tests/takeover.rs::a_trickling_client_cannot_hold_the_view |
| P-13 | Lookahead side effects | A branch sends a write, changes the main line, hangs it, or misreports what it sent. | high | mitigate | partial | crates/pageon/tests/fork.rs::writes_are_held_never_sent, ::the_main_line_is_untouched, ::lookaheads_leave_recordings_exact; crates/pageon/tests/hardening3.rs::lookahead_reads_leave_first_contact_alone, ::a_lookahead_branch_has_a_deadline |
| P-14 | Replay and reset | A reset sends a request again; a replay goes online or weakens the session. | high | mitigate | partial | crates/pageon/tests/replay.rs::resets_are_exact_and_send_nothing, ::divergences_are_explained; crates/pageon/tests/hardening.rs::live_replay_holds_recorded_writes |
| P-15 | Identity misuse | The signing key leaks, signatures are replayed or spent by pages, or a page forges signature headers. | high | mitigate | partial | crates/pageon/tests/identity.rs::the_identity_commands, ::recordings_never_hold_signatures_or_keys, ::https_signatures_verify_independently, ::http_requests_carry_no_signature_and_pages_cannot_forge_one |
| P-16 | robots.txt and refusal integrity | A page or server bypasses robots.txt or spoofs a refusal. | medium | mitigate | partial | crates/pageon/tests/identity.rs::robots_txt_is_honoured_for_the_pages_an_agent_opens, ::refusals_are_typed_everywhere; crates/pageon/tests/mcp.rs::refusals_reach_the_agent; crates/pageon/tests/hardening3.rs::robots_txt_fails_closed_and_decodes_paths; crates/pageon-net/src/refusal.rs::retry_after_is_capped_at_an_hour |
| P-17 | Page tool poisoning (WebMCP) | A page or third-party script registers tools whose names, descriptions, schemas, provenance or results mislead the agent. | high | mitigate | partial | crates/pageon/tests/webmcp.rs::hints_only_tighten, ::not_in_insecure_contexts, ::tools_are_called_as_actions; crates/pageon/tests/hardening3.rs::tool_schemas_cannot_write_view_lines; crates/pageon/tests/webmcp.rs::tools_are_called_as_actions |
| P-18 | Guard integrity | The guard sees the wrong effect, page-authored text, or secrets, or stalls. It fails closed. | medium | mitigate | partial | crates/pageon/tests/guard.rs::command_guards_speak_json_lines_and_fail_closed; crates/pageon/tests/guard.rs::guard_verdicts_must_match_their_effect, ::the_guard_sees_handles_and_clipped_page_text |
| P-19 | Script loading | Inserted, module or imported scripts run where HTML would not run them, or escape the policy. | high | mitigate | partial | crates/pageon/tests/modules.rs::modules_import_maps_and_inserted_scripts_follow_html, ::late_import_maps_merge_and_refused_imports_say_why; crates/pageon/tests/hardening.rs::cloned_inert_scripts_stay_inert; crates/pageon/tests/hardening3.rs::a_document_has_module_and_request_caps |
| P-20 | Resource exhaustion | A page or a client exhausts memory, time or requests. | medium | mitigate | partial | crates/pageon-js (time, heap and task limits; see its README); crates/pageon/tests/hardening.rs::deeply_nested_pages_render; crates/pageon/tests/cdp.rs::idle_connections_and_quoted_commands_cannot_stop_the_endpoint; crates/pageon/tests/hardening3.rs::an_action_reports_at_most_twenty_dialogs_and_popups, ::a_silent_content_process_times_out; crates/pageon/src/paint/mod.rs::paints_stay_within_the_pixel_budget; crates/pageon/src/mcp.rs::long_lines_are_refused_and_skipped |
| P-21 | Web isolation | A page reads another origin's data, steals or plants cookies, or loads mixed content. | high | mitigate | mitigated | crates/pageon/tests/network.rs::us1_fetch_and_cors_matrix, ::us2_public_suffix_and_prefix_rejections_are_logged, ::us2_partitioned_cookies_stay_with_their_top_level_site, ::us3_mixed_content_is_blocked; crates/pageon-net/tests/http_state.rs |
| P-22 | Shared helper realms | A page tampers with code a protocol client runs in CDP's utility world or a BiDi sandbox, through shared JavaScript globals. Isolated worlds are separate V8 contexts over the same document, and objects never cross worlds. | medium | mitigate | mitigated | crates/pageon-js/tests/worlds.rs; crates/pageon/tests/cdp.rs::isolated_worlds_are_separate_javascript_over_one_document; conformance/puppeteer/test/worlds.test.mjs; conformance/playwright/test/worlds.test.mjs |
| P-23 | Web components | Shadow and slotted content reaches the agent unquoted or unmarked; component styles or crafted selectors escape their tree and falsify views and actionability; repeated ids across shadow trees borrow labels (a field named by another component's label); closed shadow internals leak to page scripts through retargeting gaps; custom element callbacks loop without bound. | high | mitigate | mitigated | crates/pageon/tests/components.rs::components_render_in_views, ::shadow_labels_name_their_own_inputs, ::shadow_elements_get_the_same_actionability_checks, ::the_platform_checks_pass, ::runaway_reactions_are_bounded; crates/pageon-layout/tests/shadow.rs; crates/pageon-layout/src/scope.rs (tests) |
| P-24 | Harness code execution | Model output that a harness runs on the agent host, outside the cage. BrowserGym's `PageonEnv.step` runs the action string with Python's `exec` and full builtins, as BrowserGym does with Chromium, so a hijacked model's `__import__('os').system(…)` ran on the host. *Found by reading code, then fixed:* actions are parsed with `ast`, and only calls to action functions with literal arguments run. | high | mitigate | mitigated | bindings/python/tests/test_actions.py::test_anything_but_action_calls_runs_nothing, ::test_browsergym_action_strings_run_in_order |
| P-25 | Action target integrity | An action lands on another element than the one named: a page reacts to the scroll before an action and covers the target, a decoy with the same role and name is inserted ahead of a workflow's locator, or a hit test through frames misreports. Actions re-check the target after scrolling and refuse covered ones; workflow locators do not pin the count of equal elements. | medium | mitigate | partial | crates/pageon/tests/components.rs::shadow_elements_get_the_same_actionability_checks; crates/pageon/tests/actions.rs; crates/pageon/tests/scrolling.rs::a_cover_shown_while_scrolling_into_view_refuses_the_action |
| P-26 | Terminal output injection | Page text with control characters (ESC, OSC) reaches the operator's terminal raw through `pageon fetch` text and html dumps. *Reproduced, then fixed:* `pageon fetch` escapes control characters other than newline and tab. | low | mitigate | mitigated | crates/pageon/tests/cli.rs::page_text_cannot_drive_the_terminal |
| P-27 | Session history manipulation | A page uses the History API to show the agent a URL of another site, to flood the history and bury the agent's way back, or to trap `back` with entries it adds on its own. | medium | mitigate | mitigated | crates/pageon/tests/platform.rs::a_page_cannot_flood_or_trap_the_history, ::single_page_apps_route_in_place_and_back; crates/pageon/tests/fixtures/platform/checks.html |
| P-28 | Predictable page randomness | `crypto.getRandomValues` and `randomUUID` values (a page's tokens, nonces, PKCE verifiers) are predictable from a deterministic or weakly seeded generator. They come from a per-document ChaCha20 stream keyed with 256 bits from the OS (live) or the recording (replay); recordings therefore hold what reproduces them and stay secret (P-08). | high | mitigate | mitigated | crates/pageon/tests/platform.rs::crypto_values_differ_live_and_replay_exactly |
| P-29 | Cross-frame isolation | A cross-origin frame (an ad, a hostile widget) reaches the host's or a sibling's objects, document or nodes. Or it reads a payment field, receives a message meant for another origin, forges `event.origin`, or navigates the top page. Documents of other origins run in separate engines and see only remote windows (HTML's cross-origin members; `SecurityError` otherwise; location writes refused). `targetOrigin` is checked at delivery and `event.origin` comes from the sender's realm. Wrappers of another realm are refused before their slot is read. The mailbox and pump rounds are bounded. | high | mitigate | mitigated | crates/pageon/tests/frames_script.rs::the_cross_frame_checks_pass, ::the_cross_frame_checks_pass_caged, ::an_embedded_payment_field_talks_to_its_host, ::a_message_flood_is_bounded, ::frames_answering_each_other_forever_do_not_hold_an_action |
| P-30 | Form validation and element internals | A page plants instructions in a validation message (`setCustomValidity`, a component's `setValidity`, a pattern `title`) that the action outcome hands to the agent; a message quotes a typed secret; or a page grows custom states, form values or adopted stylesheet text without bound. Page-set messages appear in outcomes only as quoted page data clipped to 300 characters; pageon's own messages never quote a control's value, and every message passes the secret redaction; custom states are capped at 1024 per element and form values plus sheet text at 16 MiB per document, with one page error each. | medium | mitigate | mitigated | crates/pageon/tests/internals.rs::page_messages_are_page_data_and_secrets_stay_out, ::side_tables_are_capped; crates/pageon-dom/tests/validity.rs::types_patterns_and_lengths |
| P-31 | Operator files uploaded to a page | A hijacked agent or a protocol client names a host path (an SSH key, a password store); a page persuades the agent to upload a staged file into an attacker frame; a compromised content process asks for staged files outside an upload; a hostile download is re-uploaded elsewhere. Uploads (into file inputs, choosers or drop zones) take staged names only (CDP/BiDi: a staged path); the broker reads a file only for the upload action or protocol call naming it, once, for a frame of a bound origin, refusing directories and files over 64 MiB; download handles upload only to their own origin. Residual: the content process names the frame's origin (as for secrets), and a bound page can send the file's content onwards (origin sets bound it). | high | mitigate | mitigated | crates/pageon/src/files.rs::tests::only_staged_files_for_their_origins; crates/pageon/src/sandbox.rs::tests::files_only_for_the_upload_once; crates/pageon/tests/files.rs::uploads_are_refused_with_guidance, ::downloads_are_handles; crates/pageon/tests/drag.rs::files_drop_on_drop_zones, ::caged_drags_and_drops |
| P-32 | Realtime connections | A page's WebSocket frames post, bid or delete past the origin sets, guard, approver or `replay --live`'s hold; a frame carries a typed secret to another origin; a lookahead branch sends a frame or consumes the real page's messages; a socket reaches the private network (DNS rebinding); a page opens sockets without end or a server sends huge messages. Opens are reads and frames are `SEND` writes under the policy; the vault checks frames like bodies; a branch opens nothing, receives nothing and its first frame is held as pending; the broker resolves the host and connects to the vetted address; 16 connections, 4 MiB messages, 30 s handshakes. Residual: no tunnel through `--proxy` (refused); a frame navigating on its own keeps its document's sockets until the page goes. | high | mitigate | mitigated | crates/pageon/tests/realtime.rs::frames_are_writes_under_the_policy, ::secrets_do_not_leave_over_sockets, ::caged_chat_and_a_lookahead_that_would_send, ::the_private_network_and_navigation_close_connections; crates/pageon-env/tests/realtime.rs::websocket_live_recorded_and_replayed, ::connections_and_messages_are_bounded |
| P-33 | Vendor toolsets | A model (perhaps injected) calls a member the session did not offer (`javascript_exec` with the page's privileges), uploads a host file by path, reads secrets through console, network or page reads, types a vault secret into whatever has focus, clicks an overlay or a third-party frame at a coordinate believing it hit its target, forges tab lines through a page title, or has later calls of a turn run after an earlier one failed; a local process reaches the methods. Only offered members run (`javascript_exec` with `--allow-javascript-exec`, in an isolated world); `file_upload` takes staged names, refuses paths; every result is redacted, reads capped; focused typing releases a secret only for the focused frame's origin into an editable element; coordinate results name the element hit and mark third-party frames; titles and URLs cleaned of control and line separators; a turn stops at its first failure; the methods ride the MCP transport and its checks and are never in `tools/list`. Residual: a model that ignores the hit description; `find` matches words, not meaning. | high | mitigate | mitigated | crates/pageon/tests/toolsets.rs::members_the_session_did_not_offer_are_refused, ::javascript_exec_runs_in_an_isolated_world_when_allowed, ::a_turn_stops_at_its_first_failure, ::tabs_and_their_inventory, ::claude_reads_and_acts_through_the_browser_toolset, ::definitions_stay_out_of_the_tool_list |

### Open variants

- **P-02**
  - With a proxy, including `HTTP(S)_PROXY` from the environment, the proxy resolves names and
    the DNS check is skipped.
  - The DNS check is tested on the resolver, not through a request.
- **P-03** The cage allows the content process to fork (for lookahead); pageon sets no process
  or memory limit on it.
- **P-04** The content process still names the origin a secret is typed into (egress bounds
  where it can go), states each request's SameSite context (a lie sends a cookie cross-site,
  never reveals it), and holds the script-visible cookies.
- **P-05**
  - Without an approver, writes to read-write origins are sent.
  - Origin sets and the guard are opt-in.
  - The terminal approval prompt shows no body or cause and has no timeout.
  - Approvals are not recorded.
- **P-06**
  - Header values are checked raw only.
  - Bodies over 1 MiB are not decoded.
  - Transformed values (split, reversed, hex, XOR, compressed, case-changed) are not
    recognised; only origin sets stop them.
  - A script on the bound origin can send the secret onwards.
  - A secret can be typed into any editable field of its origin, not only credential fields.
- **P-07**
  - Redaction is exact-substring:
    - transformed echoes pass;
    - prefixes shorter than 4 characters pass;
    - a page logging each key event leaks the secret one character at a time.
  - The broker relays the content process's protocol output unchecked.
- **P-08** Recordings keep base64 bodies, URL-encoded secrets and passwords typed without
  `secret://` (`HttpOnly` cookie values are redacted).
- **P-09**
  - Outcome URLs, download names, error messages, popup URLs, image notes and WebMCP schema
    words are quoted and capped.
  - Content nested deeper than 400 levels is left out of the ax view without a note.
  - Tool lines and image notes are outside the token budget.
- **P-10**
  - Masking is off unless requested.
  - It ignores effective opacity, transforms, `-webkit-text-fill-color` and text inside images.
- **P-11** The endpoint's per-run secret is in the URL printed on stderr (WebSocket, or
  `pageon mcp --http`): a local
  process that can read the operator's terminal or process output, or reach `/json/*` when
  `--discovery` is on, can still connect. Such a client can also register init scripts; they
  run only in that endpoint's pages, end with them, and are never recorded or replayed.
- **P-13**
  - Writes are held by HTTP method only: a state-changing `GET` is sent (the summary counts
    it as a read).
  - A branch is stopped at its deadline (10 s) only at its next request.
  - The sandbox receipt does not mention fork.
- **P-14**
  - A tampered recording can make `replay` loop without bound.
- **P-15**
  - By default every request a page makes is signed; origin sets bound this.
  - A public constructor turns certificate checks off in the Chrome client.
  - The Chrome-profile coherence tests do not run in CI.
- **P-16**
  - Refusal markers in a `200` body raise false refusals.
- **P-17**
  - The registering script is read from the mutable `src` attribute, and stays attributed
    during microtasks.
  - `allow="tools"` ignores its allowlist and survives redirects.
  - Workflow tool steps are not pinned to the tool's provenance.
  - Requests a consequential tool starts after its call returns lose their labels.
  - Tools are not capped in number or in the budget.
- **P-18**
  - Requests started by page timers carry the agent's cause.
  - Many first contacts can each wait the full 10 s timeout.
- **P-19**
  - A later import map can redirect specifiers already resolved.
  - Cross-origin module CORS and mixed-content script refusals have no tests.
- **P-20**
  - Debug builds need a main thread's stack (8 MiB) for the parser's 512 levels; release
    builds run them on 2 MiB threads.
  - A document may make 10,000 requests (per document), all within one settle.
  - The broker's 60 s deadline counts silence: a content process that keeps sending messages
    is not cut off.

### Harness responsibilities

- **P-24** Do not `exec` model output on the host. `PageonEnv` parses actions; other harnesses
  should do the same.
- **P-05, P-09** Configure origin sets for every task that matters, and an approver or a guard
  for writes. Treat everything pageon returns from a page as untrusted, quoted or not.
- **P-08** Treat recordings and state files as secrets: store them privately and do not share
  them.
- **P-11** Keep the printed `ws://` URL private (it is the endpoint's secret); turn on
  `--discovery` only on a host without untrusted local users, and never use
  `--allow-origin '*'`.
- **P-03** Use `--no-sandbox` only with pages you trust; it runs them with your rights.
- **P-01** In code, set `LoadOptions::allow_files` to `false` when an agent chooses the URLs
  (the library allows top-level `file:` loads for operators). `PageonEnv` allows them by
  default because MiniWoB's tasks are local files; pass `allow_files=False` for web tasks.

Report a suspected vulnerability privately as described in [SECURITY.md](https://github.com/hupe1980/pageon/blob/main/SECURITY.md).
