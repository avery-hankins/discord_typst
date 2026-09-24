<p align="center">
  <img src="./logo.png" alt="logo" width="150">
</p>

# Typst Render (Discord Bot)

A Discord bot that renders [typst](https://typst.app/) markup. [Add it to your account](https://discord.com/oauth2/authorize?client_id=1521969158416236674).

## Usage

The bot registers three commands: a slash command (`/rendertypst`), a context menu command, and `/typstpackages`. The slash command opens a modal for you to type/paste your typst code, while the context menu command (used by right-clicking a message) can render code sent in someone else's message. `/typstpackages` lists the packages that render without a download.

## Packages

Any [Typst Universe](https://typst.app/universe) package can be imported:

```typst
#import "@preview/cetz:0.5.2": canvas, draw
```

A curated set is vendored into `packages/` at pinned versions and served from disk. Everything else is downloaded from the registry during the render, into memory only — so an unvendored package is re-fetched on every render, and counts against the compile timeout. `/typstpackages` shows which ones are vendored.

The tree itself isn't committed, so run `scripts/vendor-packages.sh` after a fresh checkout. To change the set, edit `packages/packages.txt` and run it again. The script refuses to finish if a vendored package imports something the list doesn't cover. Set `TYPST_PACKAGES_DIR` if the bot runs somewhere other than the repo root.

## Example
![example](./docs/example.png)

The page is automatically sized to fit your content.

<details>
<summary>Sandboxing details</summary>

The bot renders arbitrary code from non-trusted users, so each compilation job is isolated. To accomplish this, the bot has the following:

- **Separate process per render**
- **Memory cap**: each process is limited to 1GB of memory.
- **Timeout**: compiles are killed after 2 minutes.
- **No filesystem**: neither file resolver exposes the filesystem to user code — one serves the vendored `packages/` tree, the other only the Typst package registry. Local files can't be read, and `@preview` is the only namespace that resolves, so the registry is the only host user code can reach.
- **Concurrency limit**: at most 5 renders can run at the same time.

</details>
