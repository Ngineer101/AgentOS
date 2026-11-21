# Agent marks

Each agent's logo, drawn on its coloured badge in the apps (one colour, 24 × 24 view box).
From [LobeHub Icons](https://github.com/lobehub/lobe-icons) (`@lobehub/icons-static-svg`
1.104.0, MIT); the marks themselves are their owners' trademarks, used to name their agents.

- The desktop app draws these files as they are (`desktop/src/icons.rs`).
- iOS has copies in `ios/AgentHost/Assets.xcassets/Agent*.imageset/`, as template images
  that take the badge's colour. A desktop test fails when a copy differs.

Keep them to Rune's SVG subset: `<path>`s (with `fill-rule`), no `<title>`, gradients or
`em` sizes.
