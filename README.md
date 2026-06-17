# statusline

My status line for Claude Code.

![statusline](./screenshot.png)

This is my personal status line for Claude Code, built for my own setup. I'm not accepting issues, suggestions, or pull requests, feel free to fork it and customize it for your own needs.

It shows:

- Model name — `Opus 4.8 (1M context)`
- Context usage — `9%`
- Session duration — `0m`
- Input / output tokens — `92k/182`
- 5-hour usage limit and reset countdown — `5h:2% (4h17m)`
- 7-day usage limit and reset countdown — `7d:42% (2d10h)`

## Install

```sh
cargo install --path . --root ~/.local
```

Then point Claude Code at it in `settings.json`:

```json
"statusLine": {
  "type": "command",
  "command": "/Users/you/.local/bin/statusline",
  "padding": 0
}
```
