# Judge prompts for the design bench

Two prompts, written 2026-10-01 for `bench/PLAN-THE-DESIGN.md` (D2 the pairwise judge, D4 the critique). They are texts, not code: the bench driver
fills the `{{placeholders}}` the way `bench/judges/run11.py` does (`{{x}}` plain, `{{#x}}..{{/x}}` when non-empty, `{{^x}}..{{/x}}` when empty) and, when the
product ships them, they go into `serve/src/prompts.rs` as entries `design.pairwise`, `design.critique` and `design.rubric`.
v1 was run by `bench/design/run.py` on 2026-10-01 (72 % swap-order agreement); v2 (2026-10-05) is the text below, measured in `bench/design/out/report.md`.

## The rubric (one definition, three uses)

Plan section 1, quoted verbatim. The block between the markers is the single definition: the pairwise judge, the critique and the design kit
(D5) all embed exactly this text, and the Rust side extracts it the way `run11.py::template()` extracts a prompt from `prompts.rs`.

<!-- RUBRIC-BEGIN -->
1. **Hierarchy** — one clear focal point per view; headings, body and secondary text visibly distinct.
2. **Spacing and alignment** — a consistent scale, aligned edges, nothing cramped or floating.
3. **Colour and contrast** — a small palette used consistently; text readable (WCAG AA) on its background; states (hover, focus, disabled) distinguishable.
4. **Typography** — few sizes/weights, a real scale, line lengths and line heights that read.
5. **Polish** — consistent radii/shadows/borders, intentional empty/loading states, no default-browser leftovers, no layout shift.
<!-- RUBRIC-END -->

The JSON keys used for the five axes, in this order: `hierarchy`, `spacing`, `colour`, `typography`, `polish`.

## 1. Pairwise judge (D2)

Placeholders: `{{ask}}` the builder's ask, verbatim; `{{rubric}}` the block above; `{{a_desktop}}` `{{a_phone}}` `{{b_desktop}}` `{{b_phone}}` absolute paths of four PNGs.
The same two builds are judged twice, A and B swapped, and the two answers are mapped back to builds.

````text
You are a senior product designer comparing the VISUAL DESIGN of two web pages that were built for the same request. You only see screenshots.

THE REQUEST BOTH PAGES WERE BUILT FROM (context only - do not judge whether the feature is complete or works):
{{ask}}

THE RUBRIC (five axes):
{{rubric}}

THE SCREENSHOTS (open each image with your file tool and LOOK at it before you answer):
  Page A, desktop 1280x800:  {{a_desktop}}
  Page A, phone 390x844:     {{a_phone}}
  Page B, desktop 1280x800:  {{b_desktop}}
  Page B, phone 390x844:     {{b_phone}}

HOW TO JUDGE
- THE ORDER IS RANDOM. Which page is called A and which B was decided by a coin flip a moment ago and carries no meaning: A is not the original, not the newer one, not the better one. You will not be told who or what built either page; do not guess. Be as ready to pick B as A.
- Judge how well each page is DESIGNED, not how much it contains. A page with more elements, more text or a longer scroll is not better for that; a simpler page that is well executed beats a busier page that is not. Do not reward length, density or ambition.
- Judge only what is visible. If a screenshot does not show the feature the request describes, do not penalise the page for the missing feature - judge the visible things. A blank or broken-looking screenshot is judged as it looks.
- The desktop screenshots carry most of the weight; the phone screenshots decide close calls and catch broken layouts (overflow, overlap, tiny targets).
- WORK AXIS BY AXIS FIRST, THEN DECIDE. For each of the five axes write the evidence first: name ONE concrete visible difference between the two pages (an element, a gap in px, a colour pair, a font size) and say which page it favours. Only then give that axis's winner. An axis where you cannot name a concrete visible difference is "tie"; never decide an axis from a general impression.
- The overall winner is the page that wins more axes, weighing the desktop evidence most; it need not win every axis.
- TIE: say "tie" for the overall result when the axis wins are level, or when the differences you cited are all small ones - a few px of spacing, a slightly different grey, a different but equally clear type size - the kind a design reviewer would not hold up a release for, so that you would be equally happy to ship either page. A real tie is an honest answer and is not a failure; a forced pick between two pages you cannot separate is just a coin flip. When one page has a difference that clearly matters (broken or overflowing layout, unreadable text, no visible focal point) it wins.

ANSWER with exactly one JSON object and nothing else, the per-axis evidence BEFORE the verdicts:
{"per_axis": {"hierarchy": {"evidence": "the concrete visible difference", "winner": "A" | "B" | "tie"},
              "spacing": {"evidence": "...", "winner": "A" | "B" | "tie"},
              "colour": {"evidence": "...", "winner": "A" | "B" | "tie"},
              "typography": {"evidence": "...", "winner": "A" | "B" | "tie"},
              "polish": {"evidence": "...", "winner": "A" | "B" | "tie"}},
 "winner": "A" | "B" | "tie",
 "reason": "one or two sentences naming the axes that decided it"}
````

Harness rules around it (they are part of the method, not of the prompt):

- **Both orders, and a third call on a split (v2, `design.judge/2`, 2026-10-05).** Run (A=x, B=y) and (A=y, B=x). When the two answers name the same build, that build wins. When they disagree (or one says `tie`),
  ONE more call is made with the pictures in a hash-derived (random-looking, recorded as `order_third`) order and the majority of the three wins; three different answers are a tie. All three verdicts are recorded.
  An axis is decided by the first two orders only (agree or tie). Swap-order agreement of the overall winner **of the first two calls** is the number S2 must report (exit criterion: >= 85 %); `by_third` marks the matches the third call settled.
  v2 changed the prompt (the order is random and means nothing; evidence per axis BEFORE the verdict; `tie` defined) because v1 measured 13/18 = 72 %, every split purely positional, the judge never saying tie.
- **No builder names anywhere the judge can read.** Copy the four PNGs to neutral names in a fresh temp directory (`/home/user/xp/design/judge/<hash>/a-desktop.png`, ...); the paths above must not contain a seat, model or ask id.
- **The judge is a seat that built neither page**, is on, signed in and vision-capable (the `QA_GATE_ORDER` walk, Gemini out of quota until about 2026-10-04, Codex only for tiny calls while its week is high).
- **Parse the last JSON object** in the answer; an unparseable answer is an unread (not a tie) and is retried once.
- Cache by the hash of the four images + the ask + the judge seat + this file's version, so a re-run costs nothing.
- Ranking (Bradley-Terry per cell) and the operator calibration (D3: 30 pairs, agreement >= 80 % before any routing use) are outside this file.

## 2. Critique (D4)

Placeholders: `{{ask}}`, `{{rubric}}`, `{{desktop}}` and `{{phone}}` (absolute paths of the page's own two screenshots), `{{lint}}` (the page's `design_lint` block as JSON, the failed checks first,
each with its evidence node), `{{project_tokens}}` (the project's own design tokens / Tailwind config / component library, quoted, or empty). Output goes to the builder as the next round's brief,
so every fix has to be something a builder can do by editing the code.

````text
You are a senior product designer reviewing ONE web page that was just built. Your job is to hand the builder at most five concrete fixes that would make it look better. The builder is another engineer who will edit the code and cannot ask you questions.

THE REQUEST THE PAGE WAS BUILT FROM:
{{ask}}

THE RUBRIC (five axes):
{{rubric}}

THE SCREENSHOTS (open each image with your file tool and LOOK at it before you answer):
  desktop 1280x800:  {{desktop}}
  phone 390x844:     {{phone}}

WHAT A DETERMINISTIC LINT MEASURED ON THIS PAGE (facts, not opinions; each failed check names the element it found):
{{lint}}

{{#project_tokens}}THE PROJECT ALREADY HAS ITS OWN DESIGN SYSTEM - use it, do not replace it:
{{project_tokens}}
{{/project_tokens}}

RULES
- At most 5 fixes, most visible first. Fewer is fine. If the page already looks good, answer with an empty list - never invent a problem to fill the list.
- Every fix is concrete and checkable: name the element or region, say what is wrong, and give the change with a number or a value (a px gap, a hex colour, a contrast ratio, a font size, a width). "Make it more modern", "improve spacing", "add polish" are not fixes.
  Good: "Increase the gap between the toolbar and the product grid from about 6 px to 24 px; they read as one block." Good: "The secondary text under each product name is 2.9:1 against its card; darken it to at least #595959 (4.5:1)." Good: "Three different button styles are visible (filled, outlined, bare text); give the Add, Checkout and Clear buttons one shared style."
- Where the lint reports a failure that you can also see, fix that first and quote its evidence.
- Keep the feature, the content and the behaviour exactly as they are. No new dependencies, no external fonts or images, no rewrites of the page's structure, and nothing that only matters for another request.
- Do not praise. Do not describe what is already fine.

ANSWER with exactly one JSON object and nothing else:
{"fixes": [
  {"axis": "hierarchy" | "spacing" | "colour" | "typography" | "polish",
   "where": "the element or region",
   "problem": "what is wrong, as seen",
   "fix": "the change, with a number or value",
   "evidence": "the lint check and node, or what you saw in which screenshot"}
]}
````

Harness rules around it:

- One design round per run (plan D4); the fixes go to the builder verbatim as the next round's brief, and the page is photographed and linted again. The round is judged by the second `design_lint` score and, on the bench, by the pairwise judge against the first build - never by the critic's own say-so.
- The critic is the cheapest vision-capable seat inside the tolerance and is not the builder of the page.
- A critique that names more than five fixes, or a fix with no value in it, is rejected and asked once more; a still-invalid answer means no design round for that run.
- It never blocks delivery on taste: only a D1 failure the operator has turned into a rule (contrast, overflow) can.
