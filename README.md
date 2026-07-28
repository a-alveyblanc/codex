# Codex TUI with LaTeX rendering

This fork adds native inline and display-math rendering to the Codex terminal
UI. Finalized assistant messages are rendered with LaTeX, cached as
high-density PNGs, and placed with the Kitty graphics protocol and Unicode
placeholder cells.

For standard Codex installation, authentication, usage, and project
documentation, see the
[upstream Codex README](https://github.com/openai/codex/blob/main/README.md).
The official installers and package-manager releases install upstream Codex,
not the custom binary from this fork.

## Build this fork

Follow the
[upstream source-build prerequisites](https://github.com/openai/codex/blob/main/docs/install.md),
then build the feature branch from source:

```sh
git clone git@github.com:a-alveyblanc/codex.git
cd codex
git switch feature/tui-display-math
cd codex-rs
cargo build --release -p codex-cli --bin codex
./target/release/codex
```

The renderer currently requires Linux. The machine running Codex must provide:

- `bwrap` on `PATH`
- `latex`, `dvipng`, and `prlimit` installed under `/usr`
- Kitty or Ghostty, or the companion Neovim bridge described below

Install the distribution packages that provide those commands and verify their
locations with:

```sh
command -v bwrap latex dvipng prlimit
```

TeX runs without shell escape inside a networkless bubblewrap sandbox with
resource and time limits. If rendering fails or the terminal is unsupported,
Codex keeps the original Markdown math visible.

## Enable math rendering

Add this to `~/.codex/config.toml`:

```toml
[tui]
display_math = true
```

The renderer recognizes inline math such as `$x^2 + y^2$` and top-level display
math:

```text
$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$
```

Math inside code spans or fenced code blocks is left unchanged. Streaming
output remains ordinary Markdown; equations are rendered asynchronously after
the assistant message is finalized.

## tmux

Kitty graphics and terminal color queries must pass through tmux. Add this to
`~/.tmux.conf`:

```tmux
set -g allow-passthrough on
```

Reload the configuration with `tmux source-file ~/.tmux.conf`, or restart the
tmux server. Codex wraps only the outer-terminal requests that need
passthrough.

## SSH and remote sessions

The TeX pipeline runs on the machine where the Codex process runs. When Codex
runs on a remote server, install the renderer dependencies on that server.
Kitty image data travels back in the terminal stream, so no X forwarding or
shared filesystem is needed.

If tmux runs on the remote host, enable `allow-passthrough` there. A single
tmux layer is supported; nested multiplexers may require additional escape
wrapping.

## Neovim terminal panes

Neovim's terminal layer does not forward Kitty image traffic by itself. The
companion plugin is maintained separately at
[`codex-kitty-bridge.nvim`](https://github.com/a-alveyblanc/codex-kitty-bridge.nvim).

The plugin uses Snacks.nvim to forward bounded direct-PNG Kitty requests from
Neovim terminal buffers to the outer terminal. See its README for the Lazy.nvim
spec, SSH environment variables, and health checks.

## Cache, resume behavior, and storage

Rendered equations are cached across conversations under:

```text
$CODEX_HOME/cache/tui-latex
```

`$CODEX_HOME` defaults to `~/.codex`. Cache keys include the formula, math
style, renderer version, resolution, and terminal foreground/background
colors. The cache is pruned to at most 64 MiB or 512 files, so it should not
grow without bound.

On resume, equations are collected into batches. With the normal terminal
reflow row cap, Codex prepares only the retained transcript tail at startup and
defers older equations until the full transcript view needs them. Setting
`tui.terminal_resize_reflow_max_rows = 0` disables that cap and eagerly
prepares the entire resumed transcript.

## Colors and high-DPI displays

At startup, Codex queries the terminal's default foreground and background
colors and renders transparent equation images with the reported foreground.
The query is sent through tmux when needed. A terminal theme change requires a
Codex restart; the changed palette produces new cache keys automatically.

Codex reads the terminal cell dimensions in pixels and keeps a 2x output raster
for Kitty or Ghostty to downsample. After changing terminal font size, monitor
scale, or display DPI, restart Codex so newly prepared equations use the current
cell metrics.

## Keeping the fork current

This repository uses `origin` for the personal fork and `upstream` for OpenAI's
repository:

```sh
git remote add upstream https://github.com/openai/codex.git
git fetch upstream
git switch feature/tui-display-math
git rebase upstream/main
git push --force-with-lease origin feature/tui-display-math
```

Resolve upstream conflicts in the focused config, renderer, startup,
terminal-color, and documentation commits separately. Rebuild and run the TUI
tests before updating the fork branch.

This repository remains licensed under the [Apache-2.0 License](LICENSE).
