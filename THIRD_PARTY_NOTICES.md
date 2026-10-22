# Third-party notices

ai-remote is licensed under the GNU AGPL v3 (see `LICENSE`). Parts of it are derived from the
projects below, under their own licenses. Vendored crates keep their notices in `vendor/`.

## Orca (stablyai/orca)

The desktop app's coding-agent chat panel (`apps/desktop/src/ui/chat/`, the row projection in
`apps/desktop/src/lib/chat.ts` and the chat section of `apps/desktop/src/styles/native.css`)
follows the layout, interaction and wording of Orca's native chat
(`src/renderer/src/components/native-chat/`, commit `ad269f4`, October 2026). No file was copied
whole; the ported components were rewritten for this app's events, strings and CSS, and each says
which Orca component it follows.

```
MIT License

Copyright (c) 2026 Lovecast Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
