These fixtures are copied from knip's `packages/knip/fixtures` at
[webpro-nl/knip@13d7f21e8ec5](https://github.com/webpro-nl/knip/tree/13d7f21e8ec5b30d8fe6d8377c9012b9db96f871/packages/knip/fixtures),
so the unused dependencies oxiclean reports can be checked against knip's
tests:

| Fixture | knip fixture and test |
|---|---|
| `knip-basic` | `dependencies/basic` |
| `knip-definitely-typed` | `dependencies/definitely-typed` |
| `knip-duplicate-dependencies` | `dependencies/duplicate-dependencies` |
| `knip-types` | `dependencies/types` |

Each `knip.json` is left out. `knip-basic/.oxicleanrc.json` is new; it sets
the entry and the ignored package that knip's `knip.json` sets. The others
use the default entries, as knip does.

knip is ISC licensed:

ISC License (ISC)

Copyright 2022-2026 Lars Kappert

Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted, provided that the above copyright notice
and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS
OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.
