# Local MiniMax Plugin V1 Reference

Use this reference only for MiniMax Code Desktop local packages installed at
`{{DATA_DIR}}/plugins/<plugin-directory>/`.

## Package structure

```text
<plugin-directory>/
  .minimax-plugin/plugin.json
  icon.png
  icon-dark.png
  servers.mcp.json
  server.js
  skills/<skill-name>/SKILL.md
  hooks/hooks.json
  scripts/hook.mjs
```

The package root is `<plugin-directory>` itself. Do not add a Marketplace catalog or another wrapper
directory.

## Package and path limits

The Desktop runtime rejects a package that exceeds any of these V1 limits:

- at most 2,048 total filesystem entries, including directories;
- at most 1,024 regular files;
- at most 16 MiB per regular file;
- at most 64 MiB across all regular files;
- at most 512 ASCII bytes in a relative file path;
- at most 128 ASCII bytes in one path segment; and
- at most 16 path segments.

Every package path must be relative and use `/` separators in the manifest. Each path segment must
match `^[A-Za-z0-9._-]+$`, must not be empty, `.` or `..`, and must not end with `.`. Rename
supplied files rather than preserving spaces, non-ASCII characters, backslashes, or other
punctuation in a package path.

Paths must also remain portable across case-insensitive filesystems. Reject any case-insensitive
path collision, including collisions between parent prefixes, such as `Skills/demo` and
`skills/demo`. Do not use a Windows reserved basename (`CON`, `PRN`, `AUX`, `NUL`, `COM1` through
`COM9`, or `LPT1` through `LPT9`), even when it has an extension or different letter case.

Only regular files and directories are allowed. Reject symbolic links, hard links, sockets, FIFOs,
devices, and other special entries. Apply these limits to the whole package, including files that
are not directly referenced by the manifest.

## Manifest

`.minimax-plugin/plugin.json` is a JSON object with no unknown fields:

```json
{
  "schemaVersion": 1,
  "name": "example-plugin",
  "displayName": "Example Plugin",
  "version": "1.0.0",
  "description": "Adds a local MCP server, reusable Skill, and lifecycle Hook.",
  "author": "User",
  "icon": "icon.png",
  "darkIcon": "icon-dark.png",
  "category": "Other",
  "exampleQueries": ["Use Example Plugin to complete this task."],
  "apps": [],
  "mcpServers": ["servers.mcp.json"],
  "skills": ["skills/example-skill/SKILL.md"],
  "hooks": ["hooks/hooks.json"]
}
```

Required fields are `"schemaVersion"`, `"name"`, `"version"`, `"description"`, `"author"`, `"icon"`,
`"category"`, `"exampleQueries"`, `"apps"`, `"mcpServers"`, and `"skills"`. `"displayName"` is
optional. `"darkIcon"` is optional and references the dark-mode logo. `"hooks"` is optional; omit it
or use an array of package-relative JSON paths. `"$schema"` is also optional and must be a string.
`schemaVersion` must be `1`; `version` must be SemVer. At least one MCP server, Skill, or executable
Hook must remain after validation; a Hook-only local Plugin is valid.

`name` and each MCP server name must match:

```text
^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$
```

`category` must be exactly one of:

```text
Office
Studio
Design & Sites
Code
Business
Sales
Productivity
Science & Healthcare
Education
Other
```

Each example query must contain non-whitespace text. `icon` must reference an existing package PNG,
JPEG, or WebP file and remains the required default/light-mode logo. Optional `darkIcon` follows the
same path and file-type rules. When the Creator uses its bundled category pool, it copies a matched
default/dark pair into the package and includes both fields. When the user supplies only a default
icon, `darkIcon` remains optional; do not synthesize or mix in an unrelated dark-mode logo. Local
App capability is not effective: keep `"apps": []`.

## MCP file

Each manifest entry ending in `.mcp.json` contains `schemaVersion` and a non-empty `mcpServers`
object. A string `$schema` is optional. MCP `timeout` values are positive integer milliseconds.

### stdio

```json
{
  "schemaVersion": 1,
  "mcpServers": {
    "example-server": {
      "type": "stdio",
      "command": "node",
      "args": ["./server.js"],
      "env": {},
      "description": "Provides example local tools.",
      "timeout": 30000
    }
  }
}
```

`command` must be a PATH-resolved executable or interpreter without `/` or `\`. Relative package
files belong in `args`. Optional `env` values must be strings.

### streamable HTTP

```json
{
  "schemaVersion": 1,
  "mcpServers": {
    "remote-example": {
      "type": "streamable-http",
      "url": "https://mcp.example.test/mcp",
      "headers": {},
      "description": "Provides remote example tools.",
      "timeout": 30000
    }
  }
}
```

### SSE

```json
{
  "schemaVersion": 1,
  "mcpServers": {
    "sse-example": {
      "type": "sse",
      "url": "https://mcp.example.test/sse",
      "headers": {},
      "description": "Provides remote SSE tools.",
      "timeout": 30000
    }
  }
}
```

Only `"stdio"`, `"streamable-http"`, and `"sse"` are accepted. Do not add `auth`, OAuth, refresh
configuration, an `"http"` alias, or unknown fields. Do not copy the `.test` example endpoints into
a real Plugin.

## Skill file

Each manifest Skill reference must have the form `skills/<skill-name>/SKILL.md`:

```markdown
---
name: example-skill
description: Perform the example workflow when the user asks for it.
---

# Example Skill

Follow the reusable workflow here.
```

The frontmatter `name` must equal `<skill-name>`, and `description` must be non-empty.

## Hook file

Each manifest Hook reference points to a JSON object containing the supported event groups and
synchronous command handlers. Read [local-plugin-hooks.md](local-plugin-hooks.md) before creating or
changing a Hook. It defines the 11 event names, handler shape, input and output boundary, process
limits, writable state directory, and cross-platform command rules.

## Unsupported content

- Local MiniMax App references are ignored; do not create `*.app.json`.
- Asynchronous Hook commands, `asyncRewake`, and non-command Hook handler types are not supported.
- Do not use paths outside the package root, symlinks, installers, native binaries, or secrets.

## Coexisting manifests

One local package may contain more than one recognized manifest. The runtime selects a valid Agent
Plugins V1 root `plugin.json` first; otherwise it selects MiniMax, then the compatible vendor
format, then Codex. An ordinary or invalid root `plugin.json` does not shadow a valid vendor
manifest. Once a vendor manifest is selected, an invalid higher-priority vendor manifest is reported
rather than silently falling through to a lower-priority one.

This Creator authors only `.minimax-plugin/plugin.json`. Preserve lower-priority manifests when
MiniMax is active. When a valid Agent Plugins V1 manifest is active, do not claim that MiniMax edits
will affect the runtime and do not remove or convert the active manifest without explicit user
approval.
