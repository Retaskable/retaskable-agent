## 1. What it is

On 2026-10-18 the Compliance tab found HSTS, CSP, Cross-Origin-Opener-Policy and Trusted Types not met on staging, and they were fixed the same day ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 7). That tab is seven of Lighthouse's audits on one page, `/`, at first load. It says itself that "Level 1 has many more requirements that aren't checked automatically", and its accessibility score is the same single page: no signed-in screen, one theme, nothing about the keyboard.

This step checks much more, still as plain code with no model:

- **Compliance**: a fixed set of probes against the staging site (headers on more than one kind of answer, TLS, cookies, CORS, error pages, caching), each mapped to the OWASP ASVS 5.0 requirement it gives evidence for.
- **Accessibility**: axe-core's WCAG 2.2 A and AA rules on every screen the scan can reach, in light and dark, at phone and desktop sizes, plus keyboard and layout checks axe can't make.

Both feed the pipeline Lighthouse uses: a finding is written up once, into a spec draft in the Inbox. Only statuses go up to the store, never the probes' output.

**What it costs.** The probes are about 30 HTTP requests, a few seconds. The sweep is kept to about a minute (section 3.3). No model runs unless something is new; then one chore-tier spec draft, well under a dollar, as for Lighthouse.

