# Plan — landing page + README media

Goal: drop the design kit at `/Users/manikandan/Downloads/ctxd` into the repo as a deployable landing page, and embed the logo + a terminal demo + the architecture diagram in the README.

## What's in the kit

| File | What it is |
|------|------------|
| `index.html` | React-on-CDN single-page landing. Loads Babel standalone, renders sections from `landing-sections.jsx`. |
| `landing-sections.jsx` | The five live components: `TerminalDemo`, `Architecture`, `McpBlock`, `Compare`, `CapabilitySection`. |
| `tokens.css`, `landing.css` | Pure-monochrome terminal/paper themes, JetBrains Mono + Newsreader. |
| `logo.jsx` | 5-node graph mark + "ctxd" wordmark. Renders monochrome via `currentColor`. |
| `favicon.svg` | Same mark, self-contained, on a black ground. |
| `tweaks-panel.jsx` | In-page tweaker for theme / density / font / hero copy. |
| `brand-kit.html` | Brand reference page. |
| `ctxd.html` (2 MB) | Self-contained build of the landing (inlined JSX). Useful as a single-file artifact. |
| `uploads/*.png` | Reference screenshots from an earlier **green-accent** variant of the hero. |

The kit ships pure monochrome (`--accent: #ffffff`). The reference screenshots show a **green-accent variant** (mint accent + olive serif italic on "memory"). One question to settle before we ship: keep the current pure mono, or restore the green accent from the screenshots? See "Open questions" below.

## What "the GIFs" actually are

There are no `.gif` files in the kit. The two animated moments live as code:

1. **Terminal demo** — a React component that types out five `ctxd` commands and their output, then loops every 4.5 s.
2. **Architecture diagram** — a hand-coded SVG with `<animateMotion>` packets flowing along the data path and a head-pointer hopping across event bricks. Already animated natively in the browser.

We need to capture both as embeddable media for the README.

## Proposed solution

### 1. Drop the kit into `site/` and deploy via GitHub Pages

- Copy the kit files into `site/` at the repo root.
- Add `.github/workflows/pages.yml` to deploy `site/` on every push to `main`.
- Configure repo settings → Pages → "Deploy from a branch: main / site/".
- First deploy lives at `https://keeprlabs.github.io/ctxd/`. Custom domain can come later.

We ship the kit as-is (React + Babel standalone via CDN). It works on first open. A future PR can bake to a real Vite bundle to drop the ~600 KB Babel runtime and improve first paint.

### 2. Render the terminal demo as a real GIF using VHS

[VHS](https://github.com/charmbracelet/vhs) is the standard for Rust CLI demo gifs (`gh`, `glow`, `bat`, etc.). It runs a `.tape` script against a real terminal and outputs `.gif`.

- Add `assets/vhs/terminal.tape` — a script that drives a real `ctxd` binary through write → read → grant → serve.
- Output to `assets/img/terminal.gif`.
- Regenerate manually via `vhs assets/vhs/terminal.tape`. Optionally wire into a workflow once we have a runner with `ctxd` installed.

The chain of trust is real: the gif shows real `ctxd` doing real things. Same script the existing demo simulates, only this one's actually executed.

### 3. Ship the architecture as a standalone animated SVG (with PNG fallback)

The architecture is already pure SVG with native browser animation. Plan:

- Extract the SVG markup from `landing-sections.jsx`, convert JSX-isms to plain XML (`className` → `class`, double-brace styles → string attrs), substitute the `var(--*)` references with hex literals from `tokens.css`.
- Save as `assets/img/architecture.svg`.
- Render a static PNG snapshot as `assets/img/architecture.png` for the rare GitHub render path that doesn't preserve `<animate>`.
- Reference via `<img src="assets/img/architecture.svg">` in the README — animation preserved on github.com when served from the repo.

### 4. README integration

On the existing `readme-rewrite` branch (PR #13), layer in:

- **Logo** — small centered SVG above the H1, like `ripgrep` / `bat` do.
- **Terminal gif** — placed inside the Quickstart section, below the code block, captioned "the same flow, run end-to-end."
- **Architecture SVG** — replaces the existing Mermaid diagram in the "How it fits" section. Higher fidelity, on-brand, animated.

Keeps the README's structure from PR #13 intact — just adds three image refs.

## Repo layout after this lands

```
/
├── README.md                       (updated — logo + 2 media refs)
├── site/                           NEW
│   ├── index.html
│   ├── tokens.css
│   ├── landing.css
│   ├── logo.jsx
│   ├── landing-sections.jsx
│   ├── tweaks-panel.jsx
│   ├── favicon.svg
│   └── brand-kit.html
├── assets/                         NEW
│   ├── img/
│   │   ├── logo.svg                (from favicon.svg, repurposed)
│   │   ├── terminal.gif            (rendered by VHS)
│   │   ├── architecture.svg        (extracted, animated)
│   │   └── architecture.png        (static fallback)
│   └── vhs/
│       └── terminal.tape           (regenerable VHS script)
└── .github/workflows/
    └── pages.yml                   NEW (deploys site/)
```

## Sequencing

We layer this on top of `readme-rewrite` (PR #13) so all README changes ship together. Two PR shape options:

**Option A — one PR (recommended).** Bundle landing + media + README into a single PR. Merge supersedes #13 with the same content plus the media additions. One review, one merge.

**Option B — two PRs.** Land #13 first (just the rewrite). Then this branch becomes a separate PR adding `site/`, `assets/`, and the media references. More steps, narrower review diffs.

Default: A unless you say otherwise.

## Open questions (please decide before execution)

1. **Theme.** Pure monochrome (current kit) or restore the green accent from your screenshots?
2. **Deploy target.** GitHub Pages on `keeprlabs/ctxd` (`https://keeprlabs.github.io/ctxd/`) is the default. Do you have a custom domain you'd like wired up (`ctxd.dev`, `ctxd.keeprlabs.org`, …)?
3. **PR shape.** A (single bundled PR, supersedes #13) or B (two PRs, #13 first)?
4. **VHS.** OK to add VHS as the way we generate the terminal gif? It needs `vhs` installed locally to regenerate; the gif itself is checked in so contributors don't need it.

## What I will NOT do without an answer to the questions above

- Touch theme tokens.
- Buy a domain or change DNS.
- Squash / supersede PR #13.

Once you answer, I execute the rest in this branch and open the PR(s).
