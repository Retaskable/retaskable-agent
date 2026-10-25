## 5. What it will likely find here

From the code as of 2026-10-20: no `Referrer-Policy` on most answers, no `Permissions-Policy`, no `security.txt`, and API answers that a browser would render. Each was a header or a file: the scan found them, and item harden-response-headers ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 7) fixed them on the site before a draft was needed.

**[Proposal]** From section 3.6's checks: only the public pages set their own title (`useDocument`, `app/src/cloud/publicPages.tsx`), so every Sample screen is called "Retaskable" and the first run will report 2.4.2 on all ten. Components from `app/src/ui` draw a focus ring, so 2.4.7 should find only pressables built elsewhere, and `Pinned`'s sticky bars are where 2.4.11 is likeliest to find something.

