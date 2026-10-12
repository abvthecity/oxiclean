These fixtures are copied unchanged from knip's `packages/knip/fixtures` at
[webpro-nl/knip@13d7f21e8ec5](https://github.com/webpro-nl/knip/tree/13d7f21e8ec5b30d8fe6d8377c9012b9db96f871/packages/knip/fixtures),
so the unused exports, types and members oxiclean reports can be checked against
knip's tests:

| Fixture | knip fixture and test |
|---|---|
| `knip-enum-members` | `types/enum-members` |
| `knip-enum-members-element-access` | `types/enum-members-element-access` |
| `knip-enum-members-element-access-resolution` | `types/enum-members-element-access-resolution` |
| `knip-enum-members-element-access-string-key` | `types/enum-members-element-access-string-key` |
| `knip-enum-members-enumerated` | `types/enum-members-enumerated` |
| `knip-exports-basic` | `exports/basic` |
| `knip-loaders` | `imports/loaders` |
| `knip-module-augmentation` | `types/declare-module-augmentation` |
| `knip-re-exports-deep` | `re-exports/deep` |
| `knip-ts-namespace` | `namespaces/ts-namespace` |
| `knip-type-visibility` | `types/type-visibility` |

Only `knip-re-exports-deep/.oxicleanrc.json` is new; it sets the entry that
knip's `package.json` sets. The others use the default entries, as knip does.

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
