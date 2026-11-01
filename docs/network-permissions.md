# Network permissions

The desktop separates answering a pending request once from creating a reusable
rule. Allow once/Deny once retain their narrow, non-sticky default. Their arrows
open one centered form with Source and Destination alongside each other and
Protocol, Port and Duration below. The final permission sentence updates with
each selection; Return confirms and Escape cancels.

| Field | Choices |
| --- | --- |
| Source | Captured executable; all applications for the captured user ID |
| Destination | Reported hostname; that hostname and its subdomains; captured IP; any destination |
| Protocol | Captured protocol; TCP and UDP |
| Port | Captured port; any port |
| Duration | Five minutes; one hour; until OpenSnitch restarts; forever |

The destination value comes from the request. Hostname and subdomain rules do
not also constrain today's resolved IP; IP rules do not also require the
hostname. Selecting subdomains explicitly includes the named hostname itself,
with literal DNS labels, escaped dots and anchored matching. A parent domain is
never inferred. Hostname attribution is reported metadata, not verification of
server identity, HTTP requests or file contents.

Every rule retains the captured user ID. All applications does not include other
users. Executable rules retain an absolute captured path, including future
binaries installed there; child executables do not inherit that path match.
Protocol and port can broaden independently of application and destination.
TCP and UDP does not authorize other protocols.

Missing executable attribution disables executable-specific permissions without
automatically selecting all applications. The user can explicitly select all
applications with a known destination. Missing hostname attribution leaves IP
rules available. All applications plus any destination is unavailable: choose
an executable or a specific destination. Unavailable choices remain visible.

## Approval contract

Existing exact action scopes are unchanged. A selectable action declares its
bounded schema as `scope: {"$selection": <Shape>}`. A human reviewer supplies
the complete selected scope in the optional `scope` field of `DecideRequest`.
The core rejects absent selections, unknown fields and values outside the
administrator-declared schema. It records the resolved concrete action in the
immutable decision. Automatic policy cannot reference selectable actions.

Producer verification checks the selected action against the offered schema
and the original request, nonce, subject/evidence digests, manifest/request
revisions and deadline. Snitchgate additionally validates the exact network
meaning against its captured connection and requires a manual human decision.
Invalid or unknown selections never become a broader fallback rule. Jev and
the second model cannot choose these fields or emit persistent policy.

The network queue retains its 14 legacy actions and adds `allow-rule` and
`deny-rule`, staying within the existing 16-action limit. Existing clients can
continue using legacy actions. Slack and terminal clients retain their existing
exact-scope controls and direct custom Source/Destination configuration to the
desktop. Generic API clients can send selected scopes; the CLI accepts
`approval decide ID allow-rule --scope-file selection.json`.

Forever persists in OpenSnitch's rules; other durations stay temporary.
Recording approval and handing off the rule remain separate from kernel
acknowledgement. Timeouts and failures continue to deny once.
