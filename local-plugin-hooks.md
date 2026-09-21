# Local MiniMax Plugin Hook Reference

Use this reference only for MiniMax-format local Plugins under
`{{DATA_DIR}}/plugins/<plugin-directory>/`. MiniMax Code also imports compatible third-party Plugin
formats, but the local Creator authors `.minimax-plugin/plugin.json` and the MiniMax wire contract.

## Manifest and Hook document

Declare one or more package-relative JSON files in the optional manifest `hooks` array:

```json
{
  "hooks": ["hooks/hooks.json"]
}
```

A Hook document contains event groups and synchronous command handlers:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "bash|write",
        "hooks": [
          {
            "type": "command",
            "command": "node \"${PLUGIN_ROOT}/scripts/pre-tool.mjs\"",
            "commandWindows": "node \"${PLUGIN_ROOT}\\scripts\\pre-tool.mjs\"",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

Only these handler fields should be authored:

- required `type: "command"`;
- required non-empty `command`;
- optional non-empty `commandWindows`, selected on Windows;
- optional integer `timeout` from 1 through 10 seconds; the default is 5 seconds.

The per-handler timeout is also bounded by one shared event budget. Ordinary events have 15 seconds
in total, while all matching `SessionEnd` handlers share a 3-second total budget. The runtime starts
at most eight commands concurrently; later batches can time out before they start. Keep the number
of matching handlers small and make every `SessionEnd` handler finish comfortably within 3 seconds,
even if its declared `timeout` is higher.

Do not create `async`, `asyncRewake`, `args`, `shell`, `if`, prompt, agent, HTTP, `mcp_tool`, or any
other handler type or field. The runtime skips an unsupported handler without disabling the Plugin's
valid MCP, Skill, or Hook capabilities. A Hook-only Plugin must still contain at least one
executable handler. One Plugin may declare at most 64 executable handlers in total.

`matcher` is optional. Omit it to run for every occurrence. When a filter is supported, prefer an
exact value or `|`-separated exact alternatives. Keep it at most 256 characters and avoid
look-around, named groups, backreferences, or nested pathological regular expressions. Matching is
case-sensitive. Tool events use the exact native names and input fields shown to the model. For
example, match `bash`, `read`, `write`, `edit`, or `task`, not Compatible aliases such as `Bash` or
`Agent`; return `updatedInput` using that same native tool schema.

## Supported events

| Event               | Trigger and main input                                                                                                          | MiniMax `matcher` target                                                                                      |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `SessionStart`      | Before the first model processing in a newly activated or resumed Hook session; input `source`                                  | `source`: `startup`, `resume`, `clear`, `compact`, or `fork`; Plugin activation is exposed as `startup`       |
| `SessionEnd`        | When the product explicitly ends the Hook session or releases it after idleness; input `reason`                                 | normalized `reason`: `clear`, `logout`, `resume`, or `other`; archive and idle timeout are exposed as `other` |
| `UserPromptSubmit`  | After the user submits a prompt and before model processing; input `prompt`                                                     | ignored; omit `matcher` because it does not filter prompt text                                                |
| `PreToolUse`        | Before a matching tool executes; inputs `tool_name`, `tool_input`, `tool_use_id`                                                | `tool_name`                                                                                                   |
| `PermissionRequest` | When a matching tool needs a permission decision; inputs `tool_name`, `tool_input`                                              | `tool_name`                                                                                                   |
| `PostToolUse`       | After a matching tool finishes successfully or unsuccessfully; inputs `tool_name`, `tool_input`, `tool_response`, `tool_use_id` | `tool_name`                                                                                                   |
| `SubagentStart`     | Before a Subagent begins model processing; inputs `agent_id`, `agent_type`                                                      | `agent_type`                                                                                                  |
| `SubagentStop`      | When a Subagent is ready to stop; inputs `agent_id`, `agent_type`, `stop_hook_active`, `last_assistant_message`                 | `agent_type`                                                                                                  |
| `Stop`              | When the root Agent is ready to stop; inputs `stop_hook_active`, `last_assistant_message`                                       | ignored; omit `matcher`                                                                                       |
| `PreCompact`        | Immediately before context compaction; input `trigger`                                                                          | `trigger`: `manual` or `auto`                                                                                 |
| `PostCompact`       | After context compaction completes; input `trigger`                                                                             | `trigger`: `manual` or `auto`                                                                                 |

Every command receives exactly one JSON object on stdin. Common fields include `hook_event_name`,
`session_id`, `cwd`, `transcript_path`, and, when available, `turn_id`, `model`, and
`permission_mode`. Treat absent optional fields as normal and ignore unknown future fields.

Lifecycle boundaries matter when choosing an event:

- `SessionStart` runs before the first model processing for a new or genuinely resumed Hook session.
  Desktop sidebar switching alone does not start or end a Hook session.
- `SessionEnd` runs for TUI `/new`, `/clear`, `/resume`, or `/fork` when leaving the old
  conversation, Desktop archive, logout, and 30 minutes of idleness. It does not run for
  conversation deletion or normal process exit.
- `PostToolUse` runs only after the tool actually starts and then completes successfully or
  unsuccessfully. A call blocked before execution, including a denied permission request, does not
  produce `PostToolUse`.
- `Stop` and `SubagentStop` run only for normal completion, not cancellation or failed execution.

## Process environment and files

The command runs with the active turn workspace as cwd, not the Plugin directory. Use the injected
environment instead of relative-path assumptions:

- `PLUGIN_ROOT` and `MINIMAX_PLUGIN_ROOT`: immutable Plugin package root;
- `PLUGIN_DATA`: writable, host-owned persistent state for this Plugin;
- `MINIMAX_PROJECT_DIR`: active turn workspace.

Prefer portable Node.js scripts. Quote every expanded path. Do not write generated state into
`PLUGIN_ROOT`, execute a package script directly through executable bits, rely on `/tmp`, or assume
Bash exists on Windows. Use `commandWindows` when the default command is not cross-platform. The
Hook runtime does not inject or guarantee a standalone Node.js executable. Resolve every external
interpreter or executable in the active runtime environment before relying on it, and run
`node --check` for Node.js scripts. Plugin recognition alone does not verify command execution.

## Output and decisions

A side-effect-only Hook should write nothing to stdout and exit with code 0. A Hook that influences
the turn writes one JSON object. `hookSpecificOutput.hookEventName` must exactly equal the active
event whenever `hookSpecificOutput` is present.

Use only the MiniMax-native outputs below:

| Event                           | Supported output                                                                                                                                                          |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `SessionStart`, `SubagentStart` | Optional `hookSpecificOutput.additionalContext`                                                                                                                           |
| `SessionEnd`, `PostCompact`     | Observation only; emit no stdout                                                                                                                                          |
| `UserPromptSubmit`              | Optional `additionalContext`, or top-level `decision: "block"` with `reason`; prompt rewriting is unsupported                                                             |
| `PreToolUse`                    | Optional complete `updatedInput`, `additionalContext`, and `permissionDecision: "allow"`, `"deny"`, or `"ask"` with an optional reason                                    |
| `PermissionRequest`             | `decision.behavior: "allow"` or `"deny"`; allow may carry complete `updatedInput`, and deny may carry `message` and `interrupt: true`; permission mutation is unsupported |
| `PostToolUse`                   | Optional `additionalContext`; tool-result replacement is unsupported                                                                                                      |
| `Stop`, `SubagentStop`          | Top-level `decision: "block"` with `reason` to continue once                                                                                                              |
| `PreCompact`                    | Top-level `decision: "block"` with `reason` to defer an ordinary compaction                                                                                               |

Inject model context:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "Use the project policy loaded by this Plugin."
  }
}
```

Block a submitted prompt:

```json
{
  "decision": "block",
  "reason": "This prompt violates the configured policy."
}
```

Deny a tool call:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "This command is blocked by the project policy."
  }
}
```

Rewrite a tool call without changing its permission decision:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "updatedInput": {
      "command": "printf safe"
    }
  }
}
```

The rewritten input is still revalidated by the tool schema and product permission/safety policy.
`permissionDecision: "allow"` is a permission-bearing decision: it can skip an ordinary permission
confirmation that the product marks Hook-auto-approvable. It does not override hard denies, explicit
ask rules, schema validation, or sandbox enforcement. Omit it for rewrite-only Hooks.

Resolve an explicit permission request:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "deny",
      "message": "This permission is blocked by the project policy."
    }
  }
}
```

For MiniMax-format Hooks, `behavior: "allow"` resolves only an ordinary permission prompt that the
product marks Hook-auto-approvable. It cannot override a hard deny, an explicit ask rule, schema
validation, or sandbox enforcement, and any `updatedInput` is revalidated. `behavior: "deny"` blocks
the request; adding `interrupt: true` also stops the active Agent run.

Add context after a tool result:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PostToolUse",
    "additionalContext": "The tool output has been checked by the Plugin."
  }
}
```

For `Stop` or `SubagentStop`, `{"decision":"block","reason":"continue instructions"}` asks the Agent
to continue once. Inspect `stop_hook_active` and keep an explicit one-shot guard in `PLUGIN_DATA` to
prevent an infinite continuation loop. For `PreCompact`, the same block shape defers an ordinary
compaction attempt; recovery compaction may still proceed. `SessionEnd` and `PostCompact` are
observation/side-effect events and should normally emit no stdout.

Do not invent additional decision fields. MiniMax-format Hooks do not support
`PermissionRequest.updatedPermissions`; the Creator must never generate permission mutations.
MiniMax-format `PostToolUse` cannot replace the tool result; it can only add model context. Imported
compatible vendor formats have separate adapters, but their vendor-only output fields are outside
this MiniMax Creator contract.

## Bounds and failure behavior

- stdin must remain at most 1 MiB;
- stdout and stderr are each capped at 64 KiB;
- injected text is capped at 65,536 characters;
- commands are killed on timeout, cancellation, or output overflow;
- malformed input/output, process failures, and unsupported handlers produce bounded diagnostics and
  normally fail open without a user-facing error.

Keep Hook work short and deterministic. Never place secrets in commands, stdout, stderr, Hook JSON,
or package files. Validate every referenced file, then let MiniMax Code's automatic local Plugin
rescan confirm that the Plugin appears without scan diagnostics.
