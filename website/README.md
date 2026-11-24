# AgentOS website

The landing page and basic docs, as plain HTML and CSS with no build step.
Cloudflare Workers serves the files in `public/` as
[static assets](https://developers.cloudflare.com/workers/static-assets/);
there is no Worker code.

| Path | What |
| --- | --- |
| `public/index.html` | Landing page: what it is, how it works, setup steps |
| `public/docs/` | Getting started, usage, remote access and iOS, security model |
| `public/styles.css` | The one stylesheet; colours follow the apps (see its first lines) |
| `public/site.js` | Copy buttons on code blocks |
| `public/img/` | Logo, agent marks and screenshots, copied from `assets/` and `docs/` |

The docs pages are a shorter version of `README.md` and `docs/*.md`; when
those change, change the pages too.

## Preview

```sh
cd website
npx wrangler dev          # http://localhost:8787
```

Any static file server works as well, for example
`python3 -m http.server -d public`.

## Deploy to your own Cloudflare account

```sh
cd website
npx wrangler login        # once, in a browser
npx wrangler deploy
```

Wrangler prints the URL, `https://agentos-website.<subdomain>.workers.dev`
(`name` in `wrangler.toml`). To serve it from a domain on your Cloudflare
account, add this to `wrangler.toml` and deploy again:

```toml
[[routes]]
pattern = "example.com"
custom_domain = true
```
