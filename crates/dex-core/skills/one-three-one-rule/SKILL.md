---
name: one-three-one-rule
description: "1-3-1 decision briefs — state the problem, give exactly three real options with trade-offs, recommend one. Use when a choice has to go to a human (\"karar lazim\", \"secenekleri sun\", \"hangisini yapalim\", \"kisa tut, karar + sonuc\") or at an architecture or tooling fork. Do NOT use when there is only one viable path; just say so."
---

# 1-3-1 decision brief

Use this when a decision belongs to a human and there is more than one sensible way forward. The goal is a brief the reader can decide from in under a minute.

## When

- A choice needs the patron's (or another human's) approval: architecture, tooling, data migration, scope, spending.
- Several approaches are viable and differ in cost, risk or reversibility.

Skip it when only one path makes sense (say so and proceed), when the decision is already made, or for debugging and factual questions.

## Shape

1. **Problem** — one sentence about the outcome needed. No tool names, no solution hidden inside it. If the sentence needs "and", split the decision.
2. **Options A, B, C** — exactly three, each a genuinely different strategy (not three settings of one idea). Per option: one line on what it is, then its main gain and its main cost or risk.
3. **Recommendation** — pick one and say why in terms of the reader's priorities. Commit; do not hedge between two.
4. **Done when** — concrete, checkable outcomes for the recommended option. Rewrite this part if the reader picks another option.

Add short next steps only if they help the reader approve. Keep the whole brief short; detail goes in a follow-up if asked.

## Check before sending

- One problem sentence, three distinct options, one recommendation.
- Every option lists a real cost; none is a strawman.
- "Done when" items can be verified, and match the recommended option.

## Example

**Problem:** Workers' questions sometimes reach the idle leader minutes late.

- **A — Reconcile turn end from the transcript.** Gain: fixes the stale state at its source. Cost: careful ordering with late hook replies.
- **B — Raise the hook timeout.** Gain: one-line change. Cost: hides slow hooks; the race still exists.
- **C — Poll for pending questions every few seconds.** Gain: simple and robust. Cost: constant background work and duplicate-delivery risk.

**Recommendation:** A. It removes the cause instead of widening a timeout, and keeps delivery event-driven.

**Done when:**
- A question raised while the leader is idle is pasted once within a few seconds, with no patron input.
- A late Stop reply cannot mark a finished turn as working again.
- Regression tests cover the timeout, late-reply and draft-protection cases.
