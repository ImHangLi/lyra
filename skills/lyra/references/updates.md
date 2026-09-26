# Agent updates

A workspace may have a code-free log view for short updates, usually `agent-updates.feed`:

```json
{"api": 1, "id": "agent-updates", "name": "Agent Updates",
 "description": "Keep short responses explicitly published by this project's agents.",
 "views": [{"id": "feed", "title": "Updates", "kind": "log", "persistence": "last"}]}
```

Publish one update per meaningful step. Use a stable item ID and the same value as the request key, so a retry never duplicates it:

```sh
cat > /tmp/update.json <<'JSON'
{"api":1,"type":"view","view_id":"feed","op":"append",
 "data":{"kind":"log","items":[{"id":"task-42-step-3","text":"Parser updated; unit checks pass; browser flow not checked.","level":"info"}]}}
JSON
lyra publish agent-updates.feed --input /tmp/update.json --request-key task-42-step-3 --json
```

- Say what was checked and what was not. An update is your report, not proof.
- Publishing needs no session and starts nothing.
- Do not call a model to summarize a transcript for an update. Ending your turn does not mean the user's task is done; do not mark it as success.
- If the host does not offer the view, skip updates; do not create the plugin unless the user wants it.
