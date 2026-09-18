<p align="center">
  <img src="./logo.png" alt="logo" width="150">
</p>

# Typst Render (Discord Bot)

A Discord bot that renders [typst](https://typst.app/) markup. [Add it to your account](https://discord.com/oauth2/authorize?client_id=1521969158416236674).

## Usage

The bot registers three commands: a slash command (`/rendertypst`), a context menu command, and `/typstpackages`. The slash command opens a modal for you to type/paste your typst code, while the context menu command (used by right-clicking a message) can render code sent in someone else's message. `/typstpackages` lists the packages you're allowed to import.

## Packages

A small set of [Typst Universe](https://typst.app/universe) packages is vendored into `packages/` and whitelisted, at pinned versions:

```typst
#import "@preview/cetz:0.5.2": canvas, draw
```

To change it, edit `packages/packages.txt`, and run `scripts/vendor-packages.sh`. The script refuses to finish if a vendored package imports something the list doesn't cover. Set `TYPST_PACKAGES_DIR` if the bot runs somewhere other than the repo root.

## Example
![example](./docs/example.png)

The page is automatically sized to fit your content.

<details>
<summary>Sandboxing details</summary>

The bot renders arbitrary code from non-trusted users, so each compilation job is isolated. To accomplish this, the bot has the following:

- **Separate process per render**
- **Memory cap**: each process is limited to 1GB of memory.
- **Timeout**: compiles are killed after 2 minutes.
- **No filesystem or network**: the Typst engine's only file resolver serves the vendored `packages/` tree, gated by a whitelist. User code can't read local files or import *arbitrary* packages.
- **Concurrency limit**: at most 5 renders can run at the same time.

</details>
