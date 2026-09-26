# Example plugins

Three plugins you can copy into your own `.mira/plugins/` and list in `.mira/workspace.json`.
`errors` is a service that follows another run's logs and collects the lines that contain "error".
`daily` is a scheduled task that drafts a short daily update from git and TODO markers; turn it on with `mira schedule daily.draft on`.
`system` is a small system monitor for macOS: CPU, memory, and disk bars and the busiest processes, refreshed every few seconds (`mira start system.watch`). See [its README](.mira/plugins/system/README.md) for the settings and how to add a metric.
All three use only the Python standard library and never use the network.
